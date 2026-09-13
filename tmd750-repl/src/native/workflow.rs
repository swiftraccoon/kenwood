//! Captured native CAT and fixed MCP lifecycle with same-endpoint recovery.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_tmd750::DvGatewayMode;
use kenwood_transport::bluetooth::BluetoothService;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::capture::{Artifacts, CaptureKind, Failure, Recorder, TranscriptSummary};
use crate::{AppResult, CommandError, output};

use super::opening::{self, Captured, History};
use super::{Backend, Endpoint, Resolved, SystemBackend, cat};

pub(crate) use cat::Scope as CatScope;

#[cfg(test)]
mod tests;

/// Host scheduling policy after the acknowledged exit, not a readiness claim.
const POST_EXIT_SETTLE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "kind", content = "scope", rename_all = "snake_case")]
pub(crate) enum Operation {
    Cat(CatScope),
    FixedMcp,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event<'a> {
    PostExitSettle {
        milliseconds: u128,
        ownership: &'a str,
    },
    PostExitSettleCompleted,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Observation {
    Cat {
        evidence: cat::Observation,
    },
    FixedMcp {
        probe: Option<crate::mcp::ProbeEvidence>,
        gateway_before: Option<u8>,
        close_error: Option<Failure>,
        transcript: TranscriptSummary,
    },
}

#[derive(Debug, Default, Serialize)]
struct WorkflowResult {
    original_endpoint: Option<Resolved>,
    original_opening: Option<History>,
    original: Option<Observation>,
    fresh_endpoint: Option<Resolved>,
    fresh_opening: Option<History>,
    fresh_cat: Option<cat::Observation>,
    settle_error: Option<Failure>,
    settle_transcript: Option<TranscriptSummary>,
}

impl WorkflowResult {
    fn succeeded(&self) -> bool {
        if !self
            .original_opening
            .as_ref()
            .is_some_and(History::succeeded)
            || self.settle_error.is_some()
        {
            return false;
        }
        match &self.original {
            Some(Observation::Cat { evidence }) => evidence.succeeded(),
            Some(Observation::FixedMcp {
                probe,
                gateway_before,
                close_error,
                transcript,
            }) => {
                probe
                    .as_ref()
                    .is_some_and(crate::mcp::ProbeEvidence::completed_with_acknowledged_exit)
                    && *gateway_before == Some(DvGatewayMode::Off.into())
                    && close_error.is_none()
                    && transcript.complete
                    && self.fresh_opening.as_ref().is_some_and(History::succeeded)
                    && self
                        .fresh_cat
                        .as_ref()
                        .is_some_and(cat::Observation::succeeded)
            }
            None => false,
        }
    }
}

async fn run_workflow(
    backend: &mut impl Backend,
    endpoint: &Endpoint,
    operation: Operation,
    original: Recorder<File>,
    mut fresh: Recorder<File>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult::default();
    let selected = opening::open_selected(
        backend,
        endpoint,
        BluetoothService::SerialPort,
        original,
        cancelled,
    )
    .await;
    result.original_opening = Some(selected.history);
    let Some(Captured {
        transport,
        resolved,
        channel,
    }) = selected.opened
    else {
        return result;
    };
    result.original_endpoint = Some(resolved);
    match operation {
        Operation::Cat(scope) => {
            result.original = Some(Observation::Cat {
                evidence: cat::observe(
                    transport,
                    cat::Request {
                        scope,
                        expected_identity: None,
                        expected_gateway: None,
                    },
                    cancelled,
                )
                .await,
            });
        }
        Operation::FixedMcp => {
            let mut pending = crate::mcp::fixed::read(
                transport,
                crate::mcp::fixed::Admission::GatewayOff,
                cancelled,
            )
            .await;
            if pending.ready_to_settle(cancelled)
                && let Err(error) = settle(backend, &mut fresh, cancelled).await
            {
                result.settle_error = Some(Failure::from_error(&error));
                result.settle_transcript = Some(fresh.summary());
            }
            // Even a cancelled or uncaptured wait must retire the original owner.
            let observed = pending.finish().await;
            let identity = observed.verified_identity(cancelled).cloned();
            result.original = Some(Observation::FixedMcp {
                probe: observed.probe.as_ref().map(Into::into),
                gateway_before: observed.gateway_mode.map(Into::into),
                close_error: observed.close_error,
                transcript: observed.transcript,
            });
            if let Some(identity) = identity
                && result.settle_error.is_none()
            {
                let selected = opening::open_selected(
                    backend,
                    endpoint,
                    BluetoothService::FixedChannel(channel),
                    fresh,
                    cancelled,
                )
                .await;
                result.fresh_opening = Some(selected.history);
                if let Some(opened) = selected.opened {
                    result.fresh_endpoint = Some(opened.resolved);
                    result.fresh_cat = Some(
                        cat::observe(
                            opened.transport,
                            cat::Request {
                                scope: CatScope::Gateway,
                                expected_identity: Some(&identity),
                                expected_gateway: Some(DvGatewayMode::Off),
                            },
                            cancelled,
                        )
                        .await,
                    );
                }
            }
        }
    }
    result
}

pub(crate) async fn settle(
    backend: &mut impl Backend,
    recorder: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    recorder.record(Event::PostExitSettle {
        milliseconds: POST_EXIT_SETTLE.as_millis(),
        ownership: "original_connection_retained_without_protocol_traffic",
    });
    recorder.synchronize()?;
    if !cancelled.load(Ordering::Relaxed) {
        backend.wait(POST_EXIT_SETTLE).await;
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native post-exit settle cancelled; no fresh open attempted",
        ));
    }
    recorder.record(Event::PostExitSettleCompleted);
    recorder.synchronize()
}

#[derive(Serialize)]
struct Report<'a> {
    format_version: u8,
    operation: Operation,
    transport: &'static str,
    requested_address: &'a str,
    helper_executable: Option<&'a Path>,
    service: &'static str,
    post_exit_service: Option<&'static str>,
    maximum_original_open_attempts: u8,
    maximum_post_exit_open_attempts: u8,
    post_exit_settle_milliseconds: u128,
    open_retry_delay_milliseconds: u128,
    open_budget_milliseconds: u128,
    cat_exchange_timeout_milliseconds: u128,
    close_budget_milliseconds: u128,
    identity_assurance: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
    cancelled: bool,
}

impl Report<'_> {
    fn publish(&self, file: &mut File) -> io::Result<()> {
        serde_json::to_writer_pretty(&mut *file, self)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()
    }

    fn print(&self) {
        for (phase, history) in [
            ("Original", self.workflow.original_opening.as_ref()),
            ("Post-exit", self.workflow.fresh_opening.as_ref()),
        ] {
            if let Some(history) = history {
                for attempt in &history.attempts {
                    if let Some(error) = &attempt.error {
                        output::line(format_args!(
                            "{phase} native opening attempt {} failed: {error}.",
                            attempt.number
                        ));
                    }
                    if let Some(error) = &attempt.interruption {
                        output::line(format_args!(
                            "{phase} native opening attempt {} interrupted: {error}.",
                            attempt.number
                        ));
                    }
                }
                for error in [&history.retry_error, &history.capture_error]
                    .into_iter()
                    .flatten()
                {
                    output::line(format_args!("{phase} native opening stopped: {error}."));
                }
            }
        }
        if let Some(endpoint) = &self.workflow.original_endpoint {
            output::line(format_args!(
                "Native Bluetooth: {}, RFCOMM channel {}.",
                endpoint.address, endpoint.rfcomm_channel
            ));
        }
        if let Some(Observation::Cat { evidence }) = &self.workflow.original {
            if let Some(identity) = &evidence.identity {
                crate::print_identity(&identity.0);
            }
            if let Some(mode) = &evidence.band_a {
                output::line(format_args!("Band A mode: {mode}."));
            }
            if let Some(mode) = &evidence.band_b {
                output::line(format_args!("Band B mode: {mode}."));
            }
            if let Some(gateway) = evidence.gateway {
                output::line(format_args!("DV Gateway raw state: {gateway}."));
            }
        }
    }
}

/// Publish native evidence without changing USB backup or readiness schemas.
pub(crate) async fn run(
    endpoint: &Endpoint,
    operation: &Operation,
    output_path: Option<&Path>,
) -> AppResult<()> {
    if !cfg!(target_os = "macos") {
        return Err(CommandError(
            "native Bluetooth is currently available only on macOS".to_owned(),
        )
        .into());
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(
        CaptureKind::NativeBluetooth,
        output_path,
        Arc::clone(&cancelled),
    )?;
    let fresh = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
    File::open(&artifacts.directory)?.sync_all()?;
    File::open(
        artifacts
            .directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    output::line(format_args!(
        "Native Bluetooth capture: {}. At most two exact-address opening attempts per phase; no serial fallback, CAT retry or MCP retry.",
        directory.display()
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = crate::mcp::finish_on_interrupt(
        run_workflow(
            &mut SystemBackend,
            endpoint,
            *operation,
            transcript,
            fresh,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let report = Report {
        format_version: 2,
        operation: *operation,
        transport: "native_bluetooth",
        requested_address: endpoint.address.as_str(),
        helper_executable: endpoint.helper.as_deref(),
        service: "serial_port_0x1101",
        post_exit_service: matches!(operation, Operation::FixedMcp)
            .then_some("fixed_previously_opened_channel"),
        maximum_original_open_attempts: opening::MAX_ATTEMPTS,
        maximum_post_exit_open_attempts: if matches!(operation, Operation::FixedMcp) {
            opening::MAX_ATTEMPTS
        } else {
            0
        },
        post_exit_settle_milliseconds: if matches!(operation, Operation::FixedMcp) {
            POST_EXIT_SETTLE.as_millis()
        } else {
            0
        },
        open_retry_delay_milliseconds: opening::RETRY_DELAY.as_millis(),
        open_budget_milliseconds: super::OPEN_BUDGET.as_millis(),
        cat_exchange_timeout_milliseconds: cat::EXCHANGE_TIMEOUT.as_millis(),
        close_budget_milliseconds: super::CLOSE_BUDGET.as_millis(),
        identity_assurance: "exact_bluetooth_address_and_cat_tuple_not_physical_unit_continuity",
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        workflow,
        signal_error,
        cancelled: cancelled.load(Ordering::Relaxed),
    };
    let report_path = directory.join("report.json");
    report.publish(&mut report_file).map_err(|error| {
        CommandError(format!(
            "native report publication failed: {error}; retain {}",
            directory.display()
        ))
    })?;
    report.print();
    output::line(format_args!(
        "Native Bluetooth report: {}.",
        report_path.display()
    ));
    if report.workflow.succeeded() && !report.cancelled && report.signal_error.is_none() {
        Ok(())
    } else {
        Err(CommandError(format!(
            "native observation incomplete; retain {}",
            directory.display()
        ))
        .into())
    }
}
