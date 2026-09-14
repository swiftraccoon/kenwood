//! Native standard-backup admission without fabricated USB readiness evidence.

use std::path::PathBuf;

use kenwood_transport::bluetooth::{BluetoothAddress, RfcommChannel};
use serde::Deserialize;

use super::{Backup, FailureEvidence, IdentityEvidence, Provenance, Snapshot, invalid};
use crate::AppResult;

#[cfg(test)]
mod tests;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationKind {
    ConfigurationBackup,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    kind: OperationKind,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    address: String,
    rfcomm_channel: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transcript {
    file: String,
    complete: bool,
    events: u64,
    // Explicit null is mandatory. Missing error evidence cannot mean success.
    #[serde(rename = "error")]
    _error: (),
}

impl Transcript {
    fn validates(&self, file: &str) -> bool {
        self.complete && self.events > 0 && self.file == file
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetryAdmission {
    NativeOpening,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpeningFailure {
    error: FailureEvidence,
    #[serde(rename = "close_error")]
    _close_error: (),
    retry_admission: RetryAdmission,
    host_retirement_confirmed: bool,
}

impl OpeningFailure {
    fn retry_admitted(&self) -> bool {
        matches!(self.retry_admission, RetryAdmission::NativeOpening)
            && self.host_retirement_confirmed
            && self.error.present()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    number: u8,
    started: bool,
    #[serde(deserialize_with = "super::required_nullable")]
    resolved: Option<Endpoint>,
    #[serde(deserialize_with = "super::required_nullable")]
    error: Option<OpeningFailure>,
    #[serde(rename = "interruption")]
    _interruption: (),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    attempts: Vec<Attempt>,
    #[serde(rename = "retry_error")]
    _retry_error: (),
    #[serde(rename = "capture_error")]
    _capture_error: (),
    transcript: Transcript,
}

impl History {
    fn validates(&self, endpoint: &Endpoint, completed: &Transcript, file: &str) -> bool {
        if !self.transcript.validates(file)
            || !completed.validates(file)
            || self.transcript.events >= completed.events
            || !(1..=2).contains(&self.attempts.len())
        {
            return false;
        }
        self.attempts.iter().enumerate().all(|(index, attempt)| {
            attempt.started
                && usize::from(attempt.number) == index + 1
                && if index + 1 == self.attempts.len() {
                    attempt.resolved.as_ref() == Some(endpoint) && attempt.error.is_none()
                } else {
                    attempt.resolved.is_none()
                        && attempt
                            .error
                            .as_ref()
                            .is_some_and(OpeningFailure::retry_admitted)
                }
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    kind: OperationKind,
    backup: Backup,
    gateway_before: u8,
    #[serde(rename = "close_error")]
    _close_error: (),
    transcript: Transcript,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CatScope {
    Gateway,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatObservation {
    scope: CatScope,
    #[serde(deserialize_with = "super::required_nullable")]
    identity: Option<IdentityEvidence>,
    #[serde(deserialize_with = "super::required_nullable")]
    gateway: Option<u8>,
    #[serde(rename = "band_a")]
    _band_a: (),
    #[serde(rename = "band_b")]
    _band_b: (),
    #[serde(deserialize_with = "super::required_nullable")]
    operation_error: Option<FailureEvidence>,
    #[serde(rename = "close_error")]
    _close_error: (),
    #[serde(rename = "capture_error")]
    _capture_error: (),
    cancelled: bool,
    transcript: Transcript,
}

impl CatObservation {
    fn validate(&self, identity: &IdentityEvidence) -> AppResult<()> {
        if let Some(error) = &self.operation_error {
            let details = std::iter::once(&error.message)
                .chain(&error.causes)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(": ");
            return invalid(&format!(
                "native backup is incomplete: fresh CAT verification failed: {details}"
            ));
        }
        if !matches!(self.scope, CatScope::Gateway)
            || self.identity.as_ref() != Some(identity)
            || self.gateway != Some(0)
            || self.cancelled
        {
            return invalid(
                "native backup is incomplete: fresh CAT identity and Gateway Off evidence must be complete, matching, and uncancelled",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Workflow {
    original_endpoint: Endpoint,
    original_opening: History,
    original: Observation,
    fresh_endpoint: Endpoint,
    fresh_opening: History,
    fresh_cat: CatObservation,
    #[serde(rename = "settle_error")]
    _settle_error: (),
    settle_transcript: Transcript,
}

/// The complete format-3 native backup report, not a native fixed-read report.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    format_version: u8,
    operation: Operation,
    transport: String,
    requested_address: String,
    #[serde(
        rename = "helper_executable",
        deserialize_with = "super::required_nullable"
    )]
    _helper_executable: Option<PathBuf>,
    service: String,
    post_exit_service: String,
    maximum_original_open_attempts: u8,
    maximum_post_exit_open_attempts: u8,
    post_exit_settle_milliseconds: u64,
    open_retry_delay_milliseconds: u64,
    open_budget_milliseconds: u64,
    cat_exchange_timeout_milliseconds: u64,
    close_budget_milliseconds: u64,
    identity_assurance: String,
    #[serde(rename = "started_at_utc")]
    _started_at_utc: String,
    #[serde(rename = "finished_at_utc")]
    _finished_at_utc: String,
    workflow: Workflow,
    #[serde(rename = "signal_error")]
    _signal_error: (),
    cancelled: bool,
}

impl Document {
    fn validate(&self) -> AppResult<Provenance> {
        if self.format_version != 3
            || !matches!(self.operation.kind, OperationKind::ConfigurationBackup)
            || self.transport != "native_bluetooth"
            || self.service != "serial_port_0x1101"
            || self.post_exit_service != "fixed_previously_opened_channel"
            || self.maximum_original_open_attempts != 2
            || self.maximum_post_exit_open_attempts != 2
            || self.post_exit_settle_milliseconds != 5_000
            || self.open_retry_delay_milliseconds != 1_000
            || self.open_budget_milliseconds != 25_000
            || self.cat_exchange_timeout_milliseconds != 1_500
            || self.close_budget_milliseconds != 2_000
            || self.identity_assurance
                != "exact_bluetooth_address_and_cat_tuple_not_physical_unit_continuity"
            || self.cancelled
        {
            return invalid("native backup requires its exact format-3 bounded lifecycle policy");
        }
        let workflow = &self.workflow;
        let original = &workflow.original;
        let identity = &original.backup.identity;
        workflow.fresh_cat.validate(identity)?;
        let address: BluetoothAddress = self.requested_address.parse()?;
        let channel = RfcommChannel::new(workflow.original_endpoint.rfcomm_channel)?;
        if address.as_str() != self.requested_address
            || workflow.original_endpoint.address != self.requested_address
            || workflow.fresh_endpoint != workflow.original_endpoint
            || !matches!(original.kind, OperationKind::ConfigurationBackup)
            || !original.backup.completed()
            || original.gateway_before != 0
            || identity.model != "TM-D750"
            || identity.firmware != "1.02"
            || identity.radio_type != "K,2,1"
            || !workflow.original_opening.validates(
                &workflow.original_endpoint,
                &original.transcript,
                "transcript.jsonl",
            )
            || !workflow.fresh_opening.validates(
                &workflow.fresh_endpoint,
                &workflow.fresh_cat.transcript,
                "post-exit-transcript.jsonl",
            )
            || !workflow
                .settle_transcript
                .validates("post-exit-transcript.jsonl")
            || workflow.settle_transcript.events >= workflow.fresh_opening.transcript.events
        {
            return invalid(
                "native backup requires complete opening histories, exit, closes, captures, and matching exact-address CAT/Gateway Off evidence",
            );
        }
        original.backup.validate_pages()?;
        Ok(Provenance::NativeBluetooth { address, channel })
    }

    pub(super) fn into_snapshot(self) -> AppResult<Snapshot> {
        let provenance = self.validate()?;
        Snapshot::from_backup(self.workflow.original.backup, provenance)
    }
}
