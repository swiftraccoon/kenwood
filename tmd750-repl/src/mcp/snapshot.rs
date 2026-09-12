//! Strict coverage reconstruction from a completed local configuration report.

use std::fs::{File, Metadata};
use std::io::Read;
use std::path::Path;

use kenwood_tmd750::memory::{FieldDescriptor, StandardConfiguration};
use kenwood_tmd750::protocol::mcp::regions;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    FirmwareIdentity, Identity, MemoryImage, MenuFieldSnapshot, RadioModel, RadioType, Region,
    SlotIndex,
};
use serde::Deserialize;

use super::reconnect_policy::{ReconnectDecision, classify};
use super::{IdentityEvidence, SegmentEvidence};
use crate::{AppResult, CommandError};

const MAX_REPORT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum SuccessfulOperation {
    Succeeded,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum MatchedOutcome {
    Matched,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum BackupOutcome {
    AwaitingCatVerification,
}

#[derive(Debug, Deserialize)]
struct Transcript {
    complete: bool,
    // Unit requires an explicit JSON null; absent evidence is not success.
    #[serde(rename = "error")]
    _error: (),
}

impl Transcript {
    const fn succeeded(&self) -> bool {
        self.complete
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Endpoint {
    path: String,
    usb_vendor_id: Option<u16>,
    usb_product_id: Option<u16>,
}

impl Endpoint {
    fn candidate(&self) -> SerialCandidate {
        SerialCandidate {
            path: self.path.clone(),
            vid: self.usb_vendor_id,
            pid: self.usb_product_id,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    endpoint: Endpoint,
    open: SuccessfulOperation,
    #[serde(deserialize_with = "required_nullable")]
    identity: Option<IdentityEvidence>,
    close: SuccessfulOperation,
}

/// Nullable evidence still has to be present in the source document.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

impl Attempt {
    fn closed_on(&self, endpoint: &Endpoint) -> bool {
        self.endpoint == *endpoint
            && matches!(self.open, SuccessfulOperation::Succeeded)
            && matches!(self.close, SuccessfulOperation::Succeeded)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enumeration {
    elapsed_milliseconds: u64,
    candidates: Vec<Endpoint>,
}

/// Recheck the recorded selection policy without making any host observations.
fn validate_enumerations(
    enumerations: &[Enumeration],
    original: &Endpoint,
    previous_elapsed: &mut u64,
) -> bool {
    let Some((last, preceding)) = enumerations.split_last() else {
        return false;
    };
    let original = original.candidate();
    for (enumeration, must_be_ready) in preceding
        .iter()
        .map(|enumeration| (enumeration, false))
        .chain(std::iter::once((last, true)))
    {
        if enumeration.elapsed_milliseconds < *previous_elapsed {
            return false;
        }
        *previous_elapsed = enumeration.elapsed_milliseconds;
        let candidates: Vec<_> = enumeration
            .candidates
            .iter()
            .map(Endpoint::candidate)
            .collect();
        let decision = classify(&original, &candidates);
        if !matches!(
            (must_be_ready, decision),
            (true, ReconnectDecision::Ready(_)) | (false, ReconnectDecision::AwaitingEndpoint)
        ) {
            return false;
        }
    }
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IdentityAssurance {
    EndpointAndCatTupleOnly,
}

/// Historical evidence has one attempt; it is never rewritten as a retry chain.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalVerification {
    identity_assurance: IdentityAssurance,
    settle_milliseconds: u64,
    enumeration_budget_milliseconds: u64,
    maximum_open_attempts: usize,
    enumerations: Vec<Enumeration>,
    transcript: Transcript,
    outcome: MatchedOutcome,
    attempt: Attempt,
}

impl HistoricalVerification {
    fn validates(&self, endpoint: &Endpoint, identity: &IdentityEvidence) -> bool {
        let mut elapsed: u64 = 0;
        matches!(
            self.identity_assurance,
            IdentityAssurance::EndpointAndCatTupleOnly
        ) && self.settle_milliseconds > 0
            && self.enumeration_budget_milliseconds > 0
            && self.maximum_open_attempts == 1
            && self.transcript.succeeded()
            && matches!(self.outcome, MatchedOutcome::Matched)
            && self.attempt.closed_on(endpoint)
            && self.attempt.identity.as_ref() == Some(identity)
            && validate_enumerations(&self.enumerations, endpoint, &mut elapsed)
            && elapsed < self.enumeration_budget_milliseconds
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetryAdmission {
    SilentIdentityTimeout,
    Terminal,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetryFailureStage {
    Identity,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureEvidence {
    message: String,
    causes: Vec<String>,
}

impl FailureEvidence {
    fn present(&self) -> bool {
        !self.message.is_empty() && self.causes.iter().all(|cause| !cause.is_empty())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum AttemptOutcome {
    Matched,
    Failed {
        stage: RetryFailureStage,
        error: FailureEvidence,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadinessAttempt {
    enumerations: Vec<Enumeration>,
    connection: Attempt,
    outcome: AttemptOutcome,
    retry_admission: RetryAdmission,
}

impl ReadinessAttempt {
    fn retry_admitted(&self) -> bool {
        matches!(
            &self.outcome,
            AttemptOutcome::Failed { stage: RetryFailureStage::Identity, error }
                if error.present()
        ) && matches!(self.retry_admission, RetryAdmission::SilentIdentityTimeout)
            && self.connection.identity.is_none()
    }

    fn matched(&self, identity: &IdentityEvidence) -> bool {
        matches!(self.outcome, AttemptOutcome::Matched)
            && matches!(self.retry_admission, RetryAdmission::Terminal)
            && self.connection.identity.as_ref() == Some(identity)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadinessVerification {
    identity_assurance: IdentityAssurance,
    settle_milliseconds: u64,
    readiness_budget_milliseconds: u64,
    retry_interval_milliseconds: u64,
    exchange_timeout_milliseconds: u64,
    attempt_allowance_milliseconds: u64,
    maximum_open_attempts: usize,
    elapsed_milliseconds: u64,
    attempts: Vec<ReadinessAttempt>,
    transcript: Transcript,
    outcome: MatchedOutcome,
}

impl ReadinessVerification {
    fn validates(&self, endpoint: &Endpoint, identity: &IdentityEvidence) -> bool {
        let Some((last, preceding)) = self.attempts.split_last() else {
            return false;
        };
        if !matches!(
            self.identity_assurance,
            IdentityAssurance::EndpointAndCatTupleOnly
        ) || self.settle_milliseconds == 0
            || self.readiness_budget_milliseconds == 0
            || self.retry_interval_milliseconds == 0
            || self.exchange_timeout_milliseconds == 0
            || u128::from(self.attempt_allowance_milliseconds)
                < u128::from(self.exchange_timeout_milliseconds) * 6
                    + super::CLOSE_TIMEOUT.as_millis()
            || self.attempt_allowance_milliseconds > self.readiness_budget_milliseconds
            || self.maximum_open_attempts != 4
            || self.attempts.len() > self.maximum_open_attempts
            || !self.transcript.succeeded()
            || !matches!(self.outcome, MatchedOutcome::Matched)
            || !preceding.iter().all(ReadinessAttempt::retry_admitted)
            || !last.matched(identity)
        {
            return false;
        }
        let mut elapsed: u64 = 0;
        self.attempts.iter().enumerate().all(|(index, attempt)| {
            let interval_observed = index == 0
                || elapsed
                    .checked_add(self.retry_interval_milliseconds)
                    .is_some_and(|earliest| {
                        attempt
                            .enumerations
                            .first()
                            .is_some_and(|first| first.elapsed_milliseconds >= earliest)
                    });
            interval_observed
                && attempt.connection.closed_on(endpoint)
                && validate_enumerations(&attempt.enumerations, endpoint, &mut elapsed)
                && elapsed
                    .checked_add(self.attempt_allowance_milliseconds)
                    .is_some_and(|reserved| reserved <= self.readiness_budget_milliseconds)
                && elapsed <= self.elapsed_milliseconds
        })
    }
}

/// The disjoint schemas reject mixed evidence instead of guessing its meaning.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Verification {
    Historical(HistoricalVerification),
    Readiness(ReadinessVerification),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Exit {
    Acknowledged,
}

#[derive(Debug, Deserialize)]
struct Backup {
    identity: IdentityEvidence,
    entry_reply: Vec<u8>,
    segments: Vec<SegmentEvidence>,
    exit: Exit,
    outcome: BackupOutcome,
    complete_configuration: bool,
}

#[derive(Debug, Deserialize)]
struct Document {
    format_version: u8,
    operation: String,
    endpoint: Endpoint,
    transcript: Transcript,
    backup: Backup,
    #[serde(rename = "open_error")]
    _open_error: (),
    #[serde(rename = "close_error")]
    _close_error: (),
    #[serde(rename = "signal_error")]
    _signal_error: (),
    post_exit_verification: Verification,
}

impl Document {
    fn validate(&self) -> AppResult<()> {
        let fresh_identity_proved = match (&self.post_exit_verification, self.format_version) {
            (Verification::Historical(verification), 3) => {
                verification.validates(&self.endpoint, &self.backup.identity)
            }
            (Verification::Readiness(verification), 4) => {
                verification.validates(&self.endpoint, &self.backup.identity)
            }
            _ => false,
        };
        if self.operation != "configuration_backup"
            || !self.transcript.succeeded()
            || !self.backup.complete_configuration
            || self.backup.entry_reply != b"0M"
            || !matches!(self.backup.exit, Exit::Acknowledged)
            || !matches!(self.backup.outcome, BackupOutcome::AwaitingCatVerification)
            || !fresh_identity_proved
        {
            return invalid(
                "requires a successful configuration backup with matching format-3 or format-4 CAT evidence and complete captures",
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

fn validate_metadata(metadata: &Metadata) -> AppResult<()> {
    if !metadata.is_file() {
        return invalid("report input must be a regular file");
    }
    if metadata.len() > MAX_REPORT_BYTES {
        return invalid("report exceeds 32 MiB");
    }
    Ok(())
}

fn read_document(reader: impl Read) -> AppResult<Document> {
    let mut bytes = Vec::new();
    let _read = reader.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > MAX_REPORT_BYTES {
        return invalid("report exceeds 32 MiB");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// An internal dense buffer whose synthetic gaps cannot be read as fields.
#[derive(Debug)]
pub(super) struct Snapshot {
    image: MemoryImage,
    pub(super) identity: Identity,
    coverage: Vec<Region>,
}

impl Snapshot {
    /// Copy complete captured standard pages without exposing synthetic image gaps.
    pub(super) fn menu_snapshot(&self) -> AppResult<MenuFieldSnapshot> {
        let pages = regions::menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .map(|page| Ok((page, self.captured_bytes(page.region())?.to_vec())))
            .collect::<AppResult<Vec<_>>>()?;
        Ok(MenuFieldSnapshot::from_pages(pages)?)
    }

    /// Borrow only the complete, actual standard-page coverage for comparison.
    pub(super) fn standard_configuration(&self) -> AppResult<StandardConfiguration<'_>> {
        let pages = regions::menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .map(|page| Ok((page, self.captured_bytes(page.region())?)))
            .collect::<AppResult<Vec<_>>>()?;
        Ok(StandardConfiguration::new(&self.identity, pages)?)
    }

    /// Load only a complete standard configuration capture from a regular file.
    ///
    /// Checks the path before opening and checks the opened file again. Ordinary
    /// symlinks to regular files are accepted; these checks reject mistaken
    /// special-file inputs but do not provide race-resistant path opening.
    /// Metadata and the actual bounded read must both fit within 32 MiB, even
    /// if the file grows after its metadata was checked.
    pub(super) fn load(path: &Path) -> AppResult<Self> {
        validate_metadata(&path.metadata()?)?;
        let file = File::open(path)?;
        validate_metadata(&file.metadata()?)?;
        Self::from_document(read_document(file)?)
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
        let endpoint = serde_json::json!({
            "path":"/dev/cu.selected-radio","usb_vendor_id":0x2166,"usb_product_id":0x9030
        });
        let segments: Vec<_> = regions::menu_regions().into_iter().flat_map(Region::pages).map(|page| {
            serde_json::json!({"address":page.address().as_u32(),"length":page.len(),"data":vec![0x42;page.len()]})
        }).collect();
        serde_json::json!({
            "format_version":4,"operation":"configuration_backup","endpoint":endpoint,
            "transcript":{"complete":true,"error":null},
            "open_error":null,"close_error":null,"signal_error":null,
            "backup":{"identity":identity,"entry_reply":[48,77],"segments":segments,
                "exit":"acknowledged","outcome":{"status":"awaiting_cat_verification"},"complete_configuration":true},
            "post_exit_verification":{
                "identity_assurance":"endpoint_and_cat_tuple_only",
                "settle_milliseconds":2000,"readiness_budget_milliseconds":60000,
                "retry_interval_milliseconds":2000,"exchange_timeout_milliseconds":1500,
                "attempt_allowance_milliseconds":11000,"maximum_open_attempts":4,
                "elapsed_milliseconds":1000,
                "transcript":{"complete":true,"error":null},"outcome":{"status":"matched"},
                "attempts":[{
                    "enumerations":[{"elapsed_milliseconds":0,"candidates":[endpoint]}],
                    "connection":{"endpoint":endpoint,"open":{"status":"succeeded"},
                        "close":{"status":"succeeded"},"identity":identity},
                    "outcome":{"status":"matched"},"retry_admission":"terminal"
                }]
            }
        })
    }

    fn replace(
        document: &mut serde_json::Value,
        pointer: &str,
        value: serde_json::Value,
    ) -> TestResult {
        *document
            .pointer_mut(pointer)
            .ok_or("fixture pointer missing")? = value;
        Ok(())
    }

    fn historical_fixture() -> AppResult<serde_json::Value> {
        let mut document = fixture();
        replace(&mut document, "/format_version", serde_json::json!(3))?;
        let endpoint = document.get("endpoint").ok_or("endpoint missing")?.clone();
        let identity = document
            .pointer("/backup/identity")
            .ok_or("identity missing")?
            .clone();
        replace(
            &mut document,
            "/post_exit_verification",
            serde_json::json!({
                "identity_assurance":"endpoint_and_cat_tuple_only",
                "settle_milliseconds":2000,"enumeration_budget_milliseconds":60000,
                "maximum_open_attempts":1,
                "enumerations":[{"elapsed_milliseconds":0,"candidates":[endpoint]}],
                "transcript":{"complete":true,"error":null},"outcome":{"status":"matched"},
                "attempt":{"endpoint":endpoint,"open":{"status":"succeeded"},
                    "close":{"status":"succeeded"},"identity":identity}
            }),
        )?;
        Ok(document)
    }

    fn retry_fixture() -> AppResult<serde_json::Value> {
        let mut document = fixture();
        let attempts = document
            .pointer_mut("/post_exit_verification/attempts")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("fixture attempts missing")?;
        let mut silent = attempts.first().ok_or("first attempt missing")?.clone();
        replace(&mut silent, "/connection/identity", serde_json::Value::Null)?;
        replace(
            &mut silent,
            "/outcome",
            serde_json::json!({
                "status":"failed","stage":"identity",
                "error":{"message":"identity reply timed out","causes":[]}
            }),
        )?;
        replace(
            &mut silent,
            "/retry_admission",
            serde_json::json!("silent_identity_timeout"),
        )?;
        attempts.insert(0, silent);
        replace(
            &mut document,
            "/post_exit_verification/attempts/1/enumerations/0/elapsed_milliseconds",
            serde_json::json!(3500),
        )?;
        replace(
            &mut document,
            "/post_exit_verification/elapsed_milliseconds",
            serde_json::json!(4500),
        )?;
        Ok(document)
    }

    fn from_json(document: serde_json::Value) -> AppResult<Snapshot> {
        Snapshot::from_document(serde_json::from_value(document)?)
    }

    #[test]
    fn regular_report_load_preserves_source_bytes() -> TestResult {
        let directory = tempfile::tempdir()?;
        for (name, document) in [
            ("historical", historical_fixture()?),
            ("readiness", fixture()),
            ("retried", retry_fixture()?),
        ] {
            let path = directory.path().join(format!("{name}.json"));
            let bytes = serde_json::to_vec(&document)?;
            std::fs::write(&path, &bytes)?;
            let snapshot = Snapshot::load(&path)?;
            assert_eq!(snapshot.identity.firmware.as_str(), "1.02");
            assert_eq!(
                std::fs::read(&path)?,
                bytes,
                "loading must not modify the {name} source report"
            );
        }
        Ok(())
    }

    #[test]
    fn directory_input_is_rejected_as_non_regular_before_reading() -> TestResult {
        let directory = tempfile::tempdir()?;
        let error = Snapshot::load(directory.path())
            .err()
            .ok_or("directory input was accepted")?;
        assert!(
            error.to_string().contains("regular file"),
            "a directory must fail the file-kind check, not a later read: {error}"
        );
        Ok(())
    }

    #[test]
    fn oversized_sparse_report_is_rejected_from_metadata() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("oversized.json");
        File::create_new(&path)?.set_len(MAX_REPORT_BYTES + 1)?;
        let error = Snapshot::load(&path)
            .err()
            .ok_or("oversized input was accepted")?;
        assert!(
            error.to_string().contains("32 MiB"),
            "oversized metadata must fail before parsing sparse contents: {error}"
        );
        Ok(())
    }

    #[test]
    fn actual_read_rejects_excess_bytes_and_stops_after_one_limit_sentinel() -> TestResult {
        let mut reader = std::io::repeat(b' ').take(MAX_REPORT_BYTES + 2);
        let error = read_document(&mut reader)
            .err()
            .ok_or("oversized read was accepted")?;
        assert!(
            error.to_string().contains("32 MiB"),
            "the actual byte bound must be checked before JSON decoding: {error}"
        );
        assert_eq!(
            reader.limit(),
            1,
            "the bounded read must consume at most the limit plus one byte"
        );
        Ok(())
    }

    #[test]
    fn valid_report_at_the_exact_read_limit_is_accepted() -> TestResult {
        let bytes = serde_json::to_vec(&fixture())?;
        let padding = MAX_REPORT_BYTES
            .checked_sub(u64::try_from(bytes.len())?)
            .ok_or("fixture unexpectedly exceeds the report limit")?;
        let reader = bytes.as_slice().chain(std::io::repeat(b' ').take(padding));
        let document = read_document(reader)?;
        document.validate()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_regular_report_is_accepted() -> TestResult {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("target.json");
        let link = directory.path().join("link.json");
        std::fs::write(&target, serde_json::to_vec(&fixture())?)?;
        std::os::unix::fs::symlink(&target, &link)?;
        let snapshot = Snapshot::load(&link)?;
        assert_eq!(
            snapshot.identity.radio_type.as_str(),
            "K,2,1",
            "regular-file symlinks must retain the same capture identity"
        );
        Ok(())
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
    fn standard_comparison_view_cannot_expose_synthetic_backing_bytes() -> TestResult {
        let mut snapshot = Snapshot::from_document(serde_json::from_value(fixture())?)?;
        assert!(
            snapshot.standard_configuration().is_ok(),
            "a complete successful capture supplies the standard comparison view"
        );
        let _removed = snapshot.coverage.pop().ok_or("fixture coverage missing")?;
        assert!(
            snapshot.standard_configuration().is_err(),
            "dense backing bytes cannot replace missing actual coverage"
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
                "/post_exit_verification/attempts/0/connection/open/status",
                serde_json::json!("failed"),
            ),
            (
                "/post_exit_verification/attempts/0/connection/close/status",
                serde_json::json!("failed"),
            ),
            (
                "/post_exit_verification/attempts/0/connection/identity/firmware",
                serde_json::json!("1.03"),
            ),
        ] {
            let mut json = fixture();
            *json.pointer_mut(pointer).ok_or("fixture path missing")? = value;
            assert!(
                from_json(json).is_err(),
                "failed evidence at {pointer} must not be promoted"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_error_evidence_is_not_assumed_successful() -> TestResult {
        for (parent, field) in [
            ("", "open_error"),
            ("", "close_error"),
            ("", "signal_error"),
            ("/transcript", "error"),
            ("/post_exit_verification/transcript", "error"),
        ] {
            for mut json in [fixture(), historical_fixture()?] {
                let _removed = json
                    .pointer_mut(parent)
                    .and_then(serde_json::Value::as_object_mut)
                    .ok_or("fixture error parent missing")?
                    .remove(field);
                assert!(
                    serde_json::from_value::<Document>(json).is_err(),
                    "missing {parent}/{field} evidence is not a null error"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn schemas_must_match_their_format_without_mixed_attempt_shapes() -> TestResult {
        for (mut document, wrong_version, unexpected_field, unexpected_value) in [
            (fixture(), 3, "attempt", serde_json::Value::Null),
            (historical_fixture()?, 4, "attempts", serde_json::json!([])),
        ] {
            let original_version = document
                .get("format_version")
                .ok_or("format missing")?
                .clone();
            replace(
                &mut document,
                "/format_version",
                serde_json::json!(wrong_version),
            )?;
            assert!(
                from_json(document.clone()).is_err(),
                "report version cannot reinterpret a different evidence shape"
            );
            replace(&mut document, "/format_version", original_version)?;
            let _replaced = document
                .get_mut("post_exit_verification")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or("verification missing")?
                .insert(unexpected_field.to_owned(), unexpected_value);
            assert!(
                from_json(document).is_err(),
                "mixed attempt shapes must not be selected opportunistically"
            );
        }
        Ok(())
    }

    #[test]
    fn every_retry_must_be_an_admitted_silent_identity_failure() -> TestResult {
        for (pointer, value) in [
            ("/connection", serde_json::Value::Null),
            ("/connection/open", serde_json::Value::Null),
            ("/connection/open/status", serde_json::json!("failed")),
            ("/connection/close", serde_json::Value::Null),
            ("/connection/close/status", serde_json::json!("failed")),
            (
                "/connection/identity",
                serde_json::json!({"model":"TM-D750","firmware":"1.02","radio_type":"K,2,1"}),
            ),
            ("/outcome", serde_json::json!({"status":"matched"})),
            ("/outcome/status", serde_json::json!("cancelled")),
            ("/outcome/stage", serde_json::json!("identity_mismatch")),
            ("/outcome/stage", serde_json::json!("open")),
            ("/outcome/stage", serde_json::json!("close")),
            ("/outcome/stage", serde_json::json!("capture")),
            ("/outcome/stage", serde_json::json!("readiness")),
            ("/outcome/error", serde_json::Value::Null),
            ("/outcome/error/message", serde_json::json!("")),
            ("/retry_admission", serde_json::json!("terminal")),
            ("/retry_admission", serde_json::json!("timeout")),
        ] {
            let mut document = retry_fixture()?;
            *document
                .pointer_mut(&format!("/post_exit_verification/attempts/0{pointer}"))
                .ok_or("retry mutation path missing")? = value;
            assert!(
                from_json(document).is_err(),
                "a later match must not erase invalid predecessor evidence at {pointer}"
            );
        }
        Ok(())
    }

    #[test]
    fn final_attempt_must_match_identity_and_forbid_further_retry() -> TestResult {
        for (pointer, value) in [
            ("/connection", serde_json::Value::Null),
            ("/connection/identity", serde_json::Value::Null),
            ("/connection/identity/firmware", serde_json::json!("1.03")),
            (
                "/outcome",
                serde_json::json!({"status":"failed","stage":"identity",
                    "error":{"message":"timeout","causes":[]}}),
            ),
            (
                "/retry_admission",
                serde_json::json!("silent_identity_timeout"),
            ),
        ] {
            let mut document = retry_fixture()?;
            *document
                .pointer_mut(&format!("/post_exit_verification/attempts/1{pointer}"))
                .ok_or("final mutation path missing")? = value;
            assert!(
                from_json(document).is_err(),
                "aggregate matched cannot replace final successful evidence at {pointer}"
            );
        }
        Ok(())
    }

    #[test]
    fn all_attempts_require_complete_endpoint_continuity_evidence() -> TestResult {
        for index in [0, 1] {
            for (pointer, value) in [
                (
                    "/connection/endpoint/path",
                    serde_json::json!("/dev/cu.other"),
                ),
                (
                    "/connection/endpoint/usb_product_id",
                    serde_json::json!(0x9032),
                ),
                (
                    "/connection/endpoint/usb_vendor_id",
                    serde_json::json!(0x1234),
                ),
                ("/enumerations", serde_json::json!([])),
                ("/enumerations/0/candidates", serde_json::json!([])),
                (
                    "/enumerations/0/candidates/0/path",
                    serde_json::json!("/dev/cu.other"),
                ),
                (
                    "/enumerations/0/candidates/0/usb_product_id",
                    serde_json::json!(0x9032),
                ),
                (
                    "/enumerations/0/elapsed_milliseconds",
                    serde_json::json!(60_000),
                ),
            ] {
                let mut document = retry_fixture()?;
                *document
                    .pointer_mut(&format!(
                        "/post_exit_verification/attempts/{index}{pointer}"
                    ))
                    .ok_or("endpoint mutation path missing")? = value;
                assert!(
                    from_json(document).is_err(),
                    "attempt {index} must retain original endpoint evidence at {pointer}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn recorded_enumerations_cannot_ignore_conflicts_or_move_backwards() -> TestResult {
        for corruption in 0..4 {
            let mut document = retry_fixture()?;
            let endpoint = document.get("endpoint").ok_or("endpoint missing")?.clone();
            let enumerations = document
                .pointer_mut("/post_exit_verification/attempts/0/enumerations")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or("enumerations missing")?;
            match corruption {
                0 => {
                    // A rejected predecessor does not authorize another poll.
                    let mut changed = endpoint;
                    replace(&mut changed, "/usb_product_id", serde_json::json!(0x9032))?;
                    enumerations.insert(
                        0,
                        serde_json::json!({
                            "elapsed_milliseconds":0,"candidates":[changed]
                        }),
                    );
                }
                1 => {
                    // A ready predecessor already authorizes an attempt.
                    enumerations.insert(
                        0,
                        enumerations.first().ok_or("enumeration missing")?.clone(),
                    );
                }
                2 => {
                    enumerations.insert(
                        0,
                        serde_json::json!({
                            "elapsed_milliseconds":1,"candidates":[]
                        }),
                    );
                }
                _ => {
                    // The second attempt shares the same clock origin.
                    replace(
                        enumerations.first_mut().ok_or("enumeration missing")?,
                        "/elapsed_milliseconds",
                        serde_json::json!(4000),
                    )?;
                }
            }
            assert!(
                from_json(document).is_err(),
                "corrupt enumeration sequence {corruption} must fail"
            );
        }
        Ok(())
    }

    #[test]
    fn absent_endpoints_then_exact_alias_pair_remain_valid_evidence() -> TestResult {
        let mut document = fixture();
        let endpoint = document.get("endpoint").ok_or("endpoint missing")?.clone();
        let mut alias = endpoint.clone();
        replace(
            &mut alias,
            "/path",
            serde_json::json!("/dev/tty.selected-radio"),
        )?;
        replace(
            &mut document,
            "/post_exit_verification/attempts/0/enumerations",
            serde_json::json!([
                {"elapsed_milliseconds":0,"candidates":[]},
                {"elapsed_milliseconds":250,"candidates":[endpoint,alias]}
            ]),
        )?;
        assert!(
            from_json(document).is_ok(),
            "one dial-in/callout service does not create a second radio"
        );
        Ok(())
    }

    #[test]
    fn attempt_cap_and_required_fields_are_not_inferred_from_a_match() -> TestResult {
        let mut document = retry_fixture()?;
        let attempts = document
            .pointer_mut("/post_exit_verification/attempts")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("attempts missing")?;
        let silent = attempts.first().ok_or("silent attempt missing")?.clone();
        attempts.insert(0, silent.clone());
        attempts.insert(0, silent.clone());
        for (index, attempt) in attempts.iter_mut().enumerate() {
            replace(
                attempt,
                "/enumerations/0/elapsed_milliseconds",
                serde_json::json!(index * 3500),
            )?;
        }
        replace(
            &mut document,
            "/post_exit_verification/elapsed_milliseconds",
            serde_json::json!(14_000),
        )?;
        assert!(
            from_json(document.clone()).is_ok(),
            "four attempts are admitted"
        );
        document
            .pointer_mut("/post_exit_verification/attempts")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("attempts missing")?
            .insert(0, silent);
        assert!(
            from_json(document).is_err(),
            "a fifth attempt is not admitted"
        );
        for (pointer, value) in [
            ("/attempts", serde_json::json!([])),
            ("/attempts", serde_json::Value::Null),
            ("/attempts/0", serde_json::Value::Null),
            ("/maximum_open_attempts", serde_json::json!(5)),
            ("/maximum_open_attempts", serde_json::json!(0)),
            ("/attempt_allowance_milliseconds", serde_json::json!(10_000)),
            ("/attempt_allowance_milliseconds", serde_json::json!(60_001)),
            (
                "/exchange_timeout_milliseconds",
                serde_json::json!(u64::MAX),
            ),
            ("/elapsed_milliseconds", serde_json::json!(-1)),
            ("/identity_assurance", serde_json::json!("physical_unit")),
        ] {
            let mut document = fixture();
            *document
                .pointer_mut(&format!("/post_exit_verification{pointer}"))
                .ok_or("policy mutation path missing")? = value;
            assert!(
                from_json(document).is_err(),
                "invalid policy evidence at {pointer}"
            );
        }
        for (parent, field) in [
            ("/post_exit_verification", "attempts"),
            ("/post_exit_verification/attempts/0", "connection"),
            ("/post_exit_verification/attempts/0/connection", "identity"),
            ("/post_exit_verification/attempts/0/connection", "close"),
            ("/post_exit_verification/attempts/0", "retry_admission"),
        ] {
            let mut document = fixture();
            let _removed = document
                .pointer_mut(parent)
                .and_then(serde_json::Value::as_object_mut)
                .ok_or("required evidence parent missing")?
                .remove(field);
            assert!(from_json(document).is_err(), "missing {parent}/{field}");
        }
        Ok(())
    }

    #[test]
    fn dispatch_requires_full_allowance_but_cleanup_elapsed_can_overrun() -> TestResult {
        let mut document = fixture();
        replace(
            &mut document,
            "/post_exit_verification/elapsed_milliseconds",
            serde_json::json!(70_000),
        )?;
        replace(
            &mut document,
            "/post_exit_verification/attempts/0/enumerations/0/elapsed_milliseconds",
            serde_json::json!(49_000),
        )?;
        assert!(
            from_json(document.clone()).is_ok(),
            "the dispatch bound does not promise an OS or durable-sync scheduling bound"
        );
        replace(
            &mut document,
            "/post_exit_verification/attempts/0/enumerations/0/elapsed_milliseconds",
            serde_json::json!(49_001),
        )?;
        assert!(
            from_json(document.clone()).is_err(),
            "dispatch must reserve the entire identity and close allowance"
        );
        replace(
            &mut document,
            "/post_exit_verification/readiness_budget_milliseconds",
            serde_json::json!(u64::MAX),
        )?;
        replace(
            &mut document,
            "/post_exit_verification/attempt_allowance_milliseconds",
            serde_json::json!(u64::MAX),
        )?;
        assert!(
            from_json(document).is_err(),
            "overflow cannot create an apparent allowance reservation"
        );
        Ok(())
    }

    #[test]
    fn a_retry_cannot_precede_its_recorded_wait_interval() -> TestResult {
        let mut document = retry_fixture()?;
        replace(
            &mut document,
            "/post_exit_verification/attempts/1/enumerations/0/elapsed_milliseconds",
            serde_json::json!(1999),
        )?;
        assert!(
            from_json(document).is_err(),
            "new enumeration cannot occur before the declared retry wait"
        );
        Ok(())
    }

    #[test]
    fn historical_failures_remain_failures_without_synthetic_recovery() -> TestResult {
        for (pointer, value) in [
            ("/attempt", serde_json::Value::Null),
            ("/attempt/identity", serde_json::Value::Null),
            ("/attempt/close/status", serde_json::json!("failed")),
            ("/attempt/identity/firmware", serde_json::json!("1.03")),
            ("/outcome", serde_json::json!({"status":"failed"})),
            ("/transcript/complete", serde_json::json!(false)),
            ("/enumerations", serde_json::json!([])),
            ("/maximum_open_attempts", serde_json::json!(4)),
        ] {
            let mut document = historical_fixture()?;
            *document
                .pointer_mut(&format!("/post_exit_verification{pointer}"))
                .ok_or("historical mutation path missing")? = value;
            assert!(
                from_json(document).is_err(),
                "failed historical evidence at {pointer}"
            );
        }
        Ok(())
    }
}
