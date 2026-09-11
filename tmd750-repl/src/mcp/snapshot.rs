//! Strict coverage reconstruction from a completed local configuration report.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use kenwood_tmd750::memory::FieldDescriptor;
use kenwood_tmd750::protocol::mcp::regions;
use kenwood_tmd750::{
    FirmwareIdentity, Identity, MemoryImage, RadioModel, RadioType, Region, SlotIndex,
};
use serde::Deserialize;

use super::{IdentityEvidence, SegmentEvidence};
use crate::{AppResult, CommandError};

const MAX_REPORT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Status {
    status: String,
}

#[derive(Debug, Deserialize)]
struct Transcript {
    complete: bool,
    error: serde_json::Value,
}

impl Transcript {
    fn succeeded(&self) -> bool {
        self.complete && self.error.is_null()
    }
}

#[derive(Debug, Deserialize)]
struct Attempt {
    open: Status,
    identity: IdentityEvidence,
    close: Status,
}

#[derive(Debug, Deserialize)]
struct Verification {
    transcript: Transcript,
    outcome: Status,
    attempt: Attempt,
}

#[derive(Debug, Deserialize)]
struct Backup {
    identity: IdentityEvidence,
    entry_reply: Vec<u8>,
    segments: Vec<SegmentEvidence>,
    exit: String,
    outcome: Status,
    complete_configuration: bool,
}

#[derive(Debug, Deserialize)]
struct Document {
    format_version: u8,
    operation: String,
    transcript: Transcript,
    backup: Backup,
    open_error: serde_json::Value,
    close_error: serde_json::Value,
    signal_error: serde_json::Value,
    post_exit_verification: Verification,
}

impl Document {
    fn validate(&self) -> AppResult<()> {
        let verification = &self.post_exit_verification;
        if self.format_version != 3
            || self.operation != "configuration_backup"
            || !self.transcript.succeeded()
            || !self.open_error.is_null()
            || !self.close_error.is_null()
            || !self.signal_error.is_null()
            || !self.backup.complete_configuration
            || self.backup.entry_reply != b"0M"
            || self.backup.exit != "acknowledged"
            || self.backup.outcome.status != "awaiting_cat_verification"
            || !verification.transcript.succeeded()
            || verification.outcome.status != "matched"
            || verification.attempt.open.status != "succeeded"
            || verification.attempt.close.status != "succeeded"
            || verification.attempt.identity != self.backup.identity
        {
            return invalid(
                "requires a successful format-3 configuration backup with matching fresh CAT and complete captures",
            );
        }
        let pages: Vec<_> = regions::menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .collect();
        if pages.len() != self.backup.segments.len()
            || !pages
                .iter()
                .zip(&self.backup.segments)
                .all(|(page, segment)| {
                    page.address().as_u32() == segment.address
                        && page.len() == segment.length
                        && segment.data.len() == segment.length
                })
        {
            return invalid(
                "configuration pages are missing, duplicated, reordered, or incorrectly sized",
            );
        }
        Ok(())
    }
}

fn invalid<T>(message: &str) -> AppResult<T> {
    Err(Box::new(CommandError(format!(
        "Invalid configuration backup: {message}"
    ))))
}

/// An internal dense buffer whose synthetic gaps cannot be read as fields.
#[derive(Debug)]
pub(super) struct Snapshot {
    image: MemoryImage,
    pub(super) identity: Identity,
    coverage: Vec<Region>,
}

impl Snapshot {
    /// Load only a complete standard configuration capture, without opening USB.
    pub(super) fn load(path: &Path) -> AppResult<Self> {
        let file = File::open(path)?;
        if file.metadata()?.len() > MAX_REPORT_BYTES {
            return invalid("report exceeds 32 MiB");
        }
        let document: Document =
            serde_json::from_reader(BufReader::new(file.take(MAX_REPORT_BYTES)))?;
        Self::from_document(document)
    }

    fn from_document(document: Document) -> AppResult<Self> {
        document.validate()?;
        let identity = Identity {
            model: RadioModel::try_from(document.backup.identity.model.as_str())?,
            firmware: FirmwareIdentity::new(&document.backup.identity.firmware)?,
            radio_type: RadioType::new(&document.backup.identity.radio_type)?,
        };
        let mut bytes = MemoryImage::blank().into_bytes();
        for segment in document.backup.segments {
            let start = usize::try_from(segment.address)?;
            bytes
                .get_mut(start..start + segment.length)
                .ok_or("configuration page exceeds image")?
                .copy_from_slice(&segment.data);
        }
        Ok(Self {
            image: MemoryImage::from_bytes(bytes)?,
            identity,
            coverage: regions::menu_regions(),
        })
    }

    /// Permit interpretation only when every byte of this field was captured.
    pub(super) fn image_for(
        &self,
        descriptor: &FieldDescriptor,
        slot: Option<SlotIndex>,
    ) -> AppResult<&MemoryImage> {
        let start = descriptor.address(slot)?.as_u32();
        let end = start
            .checked_add(u32::try_from(descriptor.codec.encoded_len())?)
            .ok_or("field range overflow")?;
        let _captured = self.captured_bytes(Region::new(start, end)?)?;
        Ok(&self.image)
    }

    /// Return actual captured bytes only, never synthetic dense-image gaps.
    pub(super) fn captured_bytes(&self, requested: Region) -> AppResult<&[u8]> {
        if !(requested.start()..requested.end()).all(|address| {
            self.coverage
                .iter()
                .any(|region| (region.start()..region.end()).contains(&address))
        }) {
            return invalid("the requested range includes unread bytes");
        }
        let start = usize::try_from(requested.start())?;
        let end = usize::try_from(requested.end())?;
        self.image
            .as_bytes()
            .get(start..end)
            .ok_or_else(|| CommandError("captured range exceeds image".to_owned()).into())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use kenwood_tmd750::memory::FieldCodec;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    pub(crate) fn fixture() -> serde_json::Value {
        let identity =
            serde_json::json!({"model":"TM-D750","firmware":"1.02","radio_type":"K,2,1"});
        let segments: Vec<_> = regions::menu_regions().into_iter().flat_map(Region::pages).map(|page| {
            serde_json::json!({"address":page.address().as_u32(),"length":page.len(),"data":vec![0x42;page.len()]})
        }).collect();
        serde_json::json!({
            "format_version":3,"operation":"configuration_backup",
            "transcript":{"complete":true,"error":null},
            "open_error":null,"close_error":null,"signal_error":null,
            "backup":{"identity":identity,"entry_reply":[48,77],"segments":segments,
                "exit":"acknowledged","outcome":{"status":"awaiting_cat_verification"},"complete_configuration":true},
            "post_exit_verification":{"transcript":{"complete":true,"error":null},"outcome":{"status":"matched"},
                "attempt":{"open":{"status":"succeeded"},"close":{"status":"succeeded"},"identity":identity}}
        })
    }

    #[test]
    fn complete_capture_preserves_identity_and_only_exposes_captured_fields() -> TestResult {
        let snapshot = Snapshot::from_document(serde_json::from_value(fixture())?)?;
        assert_eq!(snapshot.identity.firmware.as_str(), "1.02");
        let observed = FieldDescriptor::new("observed", 8, FieldCodec::Byte { min: 0, max: 255 });
        assert_eq!(
            snapshot.image_for(&observed, None)?.as_bytes().get(8),
            Some(&0x42)
        );
        let unread = FieldDescriptor::new("unread", 0, FieldCodec::Byte { min: 0, max: 255 });
        assert!(
            snapshot.image_for(&unread, None).is_err(),
            "synthetic gap bytes must remain inaccessible"
        );
        Ok(())
    }

    #[test]
    fn full_pm1_page_requires_every_byte_of_actual_coverage() -> TestResult {
        let mut snapshot = Snapshot::from_document(serde_json::from_value(fixture())?)?;
        let page = kenwood_tmd750::memory::PmNameTrial::required_page()?;
        assert_eq!(snapshot.captured_bytes(page.region())?, &[0x42; 256]);
        snapshot.coverage = vec![Region::new(323_594, 323_610)?];
        assert!(
            snapshot.captured_bytes(page.region()).is_err(),
            "field-only coverage cannot supply a whole page"
        );
        assert_eq!(
            snapshot.captured_bytes(Region::new(323_594, 323_610)?)?,
            &[0x42; 16]
        );
        Ok(())
    }

    #[test]
    fn missing_duplicate_reordered_and_truncated_pages_are_rejected() -> TestResult {
        for mutation in 0..4 {
            let mut document: Document = serde_json::from_value(fixture())?;
            match mutation {
                0 => {
                    let _removed = document.backup.segments.pop();
                }
                1 => {
                    document
                        .backup
                        .segments
                        .get_mut(1)
                        .ok_or("second page missing")?
                        .address = 8;
                }
                2 => document.backup.segments.swap(0, 1),
                _ => {
                    let _removed = document
                        .backup
                        .segments
                        .first_mut()
                        .ok_or("first page missing")?
                        .data
                        .pop();
                }
            }
            assert!(
                Snapshot::from_document(document).is_err(),
                "invalid page mutation {mutation} must fail"
            );
        }
        Ok(())
    }

    #[test]
    fn failed_capture_cleanup_signal_or_identity_cannot_be_promoted_to_a_snapshot() -> TestResult {
        for (pointer, value) in [
            ("/format_version", serde_json::json!(2)),
            ("/operation", serde_json::json!("probe")),
            ("/transcript/complete", serde_json::json!(false)),
            ("/transcript/error", serde_json::json!("disk failure")),
            ("/open_error", serde_json::json!("failed")),
            ("/close_error", serde_json::json!("failed")),
            ("/signal_error", serde_json::json!("failed")),
            ("/backup/entry_reply", serde_json::json!([0])),
            ("/backup/exit", serde_json::json!("not_acknowledged")),
            ("/backup/complete_configuration", serde_json::json!(false)),
            ("/backup/outcome/status", serde_json::json!("cancelled")),
            (
                "/post_exit_verification/transcript/complete",
                serde_json::json!(false),
            ),
            (
                "/post_exit_verification/outcome/status",
                serde_json::json!("failed"),
            ),
            (
                "/post_exit_verification/attempt/open/status",
                serde_json::json!("failed"),
            ),
            (
                "/post_exit_verification/attempt/close/status",
                serde_json::json!("failed"),
            ),
            (
                "/post_exit_verification/attempt/identity/firmware",
                serde_json::json!("1.03"),
            ),
        ] {
            let mut json = fixture();
            *json.pointer_mut(pointer).ok_or("fixture path missing")? = value;
            let document: Document = serde_json::from_value(json)?;
            assert!(
                Snapshot::from_document(document).is_err(),
                "failed evidence at {pointer} must not be promoted"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_error_evidence_is_not_assumed_successful() -> TestResult {
        let mut json = fixture();
        let _removed = json
            .as_object_mut()
            .ok_or("fixture is not object")?
            .remove("close_error");
        assert!(
            serde_json::from_value::<Document>(json).is_err(),
            "missing cleanup evidence is not a null error"
        );
        Ok(())
    }
}
