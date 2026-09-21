//! Serialized probe report: endpoint, limits, stage, outcome and transcript.
//!
//! Field names and the `kind` tags are the stable on-disk format. A recorded
//! CAT or MMDVM observation describes the one connection that answered, not
//! the radio's mode.

use kenwood_tmd750::Identity;
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate};
use mmdvm::core::{ModemStatus, VersionResponse};
use serde::Serialize;

use crate::capture::{Failure, TranscriptSummary};
use crate::output;

use super::Limits;

#[derive(Debug, Serialize)]
pub(super) struct Report {
    pub(super) format_version: u8,
    pub(super) operation: &'static str,
    pub(super) software_version: &'static str,
    pub(super) started_at_utc: String,
    pub(super) finished_at_utc: String,
    pub(super) endpoint: Endpoint,
    pub(super) limits: Limits,
    #[serde(flatten)]
    pub(super) result: WorkflowResult,
    pub(super) signal_error: Option<Failure>,
    pub(super) cancelled: bool,
}

impl Report {
    pub(super) const fn succeeded(&self) -> bool {
        !self.cancelled && self.signal_error.is_none() && self.result.succeeded()
    }

    pub(super) fn print(&self) {
        match &self.result.outcome {
            Outcome::CatObserved { identity, gateway } => output::line(format_args!(
                "CAT observed: {} firmware {}, type {}; Gateway raw value {}. No binary query was sent.",
                identity.model, identity.firmware, identity.radio_type, gateway,
            )),
            Outcome::MmdvmObserved {
                version, status, ..
            } => output::line(format_args!(
                "MMDVM version/status observed: protocol {}, description {:?}, mode {}. No modem setup was sent.",
                version.protocol, version.description, status.mode,
            )),
            Outcome::Cancelled { .. } => {
                output::line(format_args!(
                    "Diagnostic cancelled; the connection was closed."
                ));
            }
            Outcome::CatGatewayFailed { error, .. }
            | Outcome::VersionFailed { error, .. }
            | Outcome::StatusFailed { error, .. }
            | Outcome::Failed { error, .. } => {
                output::error(format_args!("Diagnostic failed: {error}"));
            }
        }
        if self.cancelled {
            output::line(format_args!(
                "Cancellation was requested; this run is incomplete."
            ));
        }
        for (stage, error) in [
            ("close", self.result.close_error.as_ref()),
            (
                "capture synchronization",
                self.result.synchronization_error.as_ref(),
            ),
            ("signal", self.signal_error.as_ref()),
        ] {
            if let Some(error) = error {
                output::error(format_args!("Diagnostic {stage} failed: {error}"));
            }
        }
        output::line(format_args!("No mode or routing change was requested."));
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Endpoint {
    path: String,
    usb_vendor_id: Option<u16>,
    usb_product_id: Option<u16>,
    baud: u32,
}

impl From<&SerialCandidate> for Endpoint {
    fn from(endpoint: &SerialCandidate) -> Self {
        Self {
            path: endpoint.path.clone(),
            usb_vendor_id: endpoint.vid,
            usb_product_id: endpoint.pid,
            baud: DEFAULT_BAUD,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct WorkflowResult {
    pub(super) outcome: Outcome,
    pub(super) close_error: Option<Failure>,
    pub(super) synchronization_error: Option<Failure>,
    pub(super) transcript: TranscriptSummary,
}

impl WorkflowResult {
    pub(super) const fn succeeded(&self) -> bool {
        self.close_error.is_none()
            && self.synchronization_error.is_none()
            && self.transcript.complete
            && matches!(
                self.outcome,
                Outcome::CatObserved { .. } | Outcome::MmdvmObserved { .. }
            )
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Outcome {
    CatObserved {
        identity: IdentityEvidence,
        gateway: u8,
    },
    CatGatewayFailed {
        identity: IdentityEvidence,
        error: Failure,
    },
    MmdvmObserved {
        cat_silence: Failure,
        version: VersionEvidence,
        status: StatusEvidence,
    },
    VersionFailed {
        cat_silence: Failure,
        error: Failure,
    },
    StatusFailed {
        cat_silence: Failure,
        version: VersionEvidence,
        error: Failure,
    },
    Cancelled {
        identity: Option<IdentityEvidence>,
        version: Option<VersionEvidence>,
    },
    Failed {
        stage: Stage,
        error: Failure,
    },
}

impl Outcome {
    pub(super) fn failed(stage: Stage, error: &(dyn std::error::Error + 'static)) -> Self {
        Self::Failed {
            stage,
            error: Failure::from_error(error),
        }
    }

    pub(super) const fn cancelled() -> Self {
        Self::Cancelled {
            identity: None,
            version: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Stage {
    Open,
    Capture,
    CatIdentity,
}

#[derive(Debug, Serialize)]
pub(super) struct IdentityEvidence {
    model: String,
    firmware: String,
    radio_type: String,
}

impl From<&Identity> for IdentityEvidence {
    fn from(identity: &Identity) -> Self {
        Self {
            model: identity.model.to_string(),
            firmware: identity.firmware.to_string(),
            radio_type: identity.radio_type.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct VersionEvidence {
    protocol: u8,
    description: String,
    capabilities: Option<[u8; 2]>,
}

impl From<&VersionResponse> for VersionEvidence {
    fn from(version: &VersionResponse) -> Self {
        Self {
            protocol: version.protocol,
            description: version.description.clone(),
            capabilities: version.capabilities.map(|value| [value.cap1, value.cap2]),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct StatusEvidence {
    mode: u8,
    decoded_flags: u8,
    dstar_space: u8,
    dmr_space1: u8,
    dmr_space2: u8,
    ysf_space: u8,
    p25_space: u8,
    nxdn_space: u8,
    pocsag_space: u8,
    fm_space: u8,
}

impl From<ModemStatus> for StatusEvidence {
    fn from(status: ModemStatus) -> Self {
        Self {
            mode: status.mode.as_byte(),
            decoded_flags: status.flags.bits(),
            dstar_space: status.dstar_space,
            dmr_space1: status.dmr_space1,
            dmr_space2: status.dmr_space2,
            ysf_space: status.ysf_space,
            p25_space: status.p25_space,
            nxdn_space: status.nxdn_space,
            pocsag_space: status.pocsag_space,
            fm_space: status.fm_space,
        }
    }
}
