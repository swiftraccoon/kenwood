//! Complete historical byte comparison with finite, explicitly qualified labels.

mod catalog;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Args;
use kenwood_tmd750::SlotIndex;
use kenwood_tmd750::memory::{
    ReflectorTerminalPreflight, StandardConfigurationDiff, TerminalUsbRoute,
    TextLayoutQualification, is_supported_schema_target,
};
use serde::Serialize;

use self::catalog::{Catalog, FieldLocation};
use super::super::{IdentityEvidence, write_report};
use super::{Snapshot, UsbInterface, assess_snapshot, parse_slot};
use crate::{AppResult, CommandError, capture, output};

/// Compare complete configuration captures without selecting or opening a radio.
#[derive(Debug, Args)]
pub(super) struct CompareRequest {
    /// Complete successful configuration backup to treat as the earlier state.
    #[arg(long, value_name = "REPORT")]
    before: PathBuf,

    /// Complete successful configuration backup to treat as the later state.
    #[arg(long, value_name = "REPORT")]
    after: PathBuf,

    /// PM slot for both preflights; byte comparison still includes every slot.
    #[arg(long, value_parser = parse_slot, value_name = "0..5")]
    slot: SlotIndex,

    /// Intended USB route for assessment only, not endpoint selection.
    #[arg(long, value_enum)]
    interface: UsbInterface,

    /// Explicitly annotate unqualified firmware layouts; required for 1.02.
    #[arg(long)]
    interpret_unqualified: bool,

    /// New private JSON report with exact byte changes and interpreted text.
    #[arg(long, value_name = "NEW_REPORT.json")]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct ComparisonReport {
    format_version: u8,
    operation: &'static str,
    software_version: &'static str,
    qualification: &'static str,
    requested_slot: u8,
    requested_interface: String,
    before: SnapshotEvidence,
    after: SnapshotEvidence,
    compared_pages: usize,
    compared_bytes: usize,
    changed_bytes: usize,
    outside_terminal_catalog_bytes: usize,
    pages: Vec<PageChange>,
    fields: Vec<FieldChange>,
    radio_accessed: bool,
    radio_applied: bool,
    limitations: [&'static str; 4],
}

#[derive(Debug, Serialize)]
struct SnapshotEvidence {
    path: PathBuf,
    identity: IdentityEvidence,
    assessment: AssessmentEvidence,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AssessmentEvidence {
    Decoded {
        captured_active_slot: u8,
        usb_function: String,
        gateway_route: String,
        gateway_mode: String,
        terminal_mode: String,
        selected_my_entry: u8,
        my_callsign: String,
        rpt1: String,
        rpt2: String,
        findings: Vec<String>,
    },
    Failed {
        error: String,
    },
}

impl From<ReflectorTerminalPreflight> for AssessmentEvidence {
    fn from(value: ReflectorTerminalPreflight) -> Self {
        Self::Decoded {
            captured_active_slot: value.captured_active_slot.index(),
            usb_function: value.usb_function.to_string(),
            gateway_route: value.gateway_route.to_string(),
            gateway_mode: value.gateway_mode.to_string(),
            terminal_mode: value.terminal_mode.to_string(),
            selected_my_entry: value.selected_my_callsign.index() + 1,
            my_callsign: value.my_callsign,
            rpt1: value.rpt1,
            rpt2: value.rpt2,
            findings: value.findings.iter().map(ToString::to_string).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct PageChange {
    address: u32,
    length: usize,
    changes: Vec<ByteChange>,
}

#[derive(Debug, Serialize)]
struct ByteChange {
    address: u32,
    before: u8,
    after: u8,
}

#[derive(Debug, Serialize)]
struct FieldChange {
    field: FieldLocation,
    changed_bytes: usize,
}

fn snapshot_evidence(
    path: PathBuf,
    snapshot: &Snapshot,
    request: &CompareRequest,
) -> SnapshotEvidence {
    let assessment = assess_snapshot(
        snapshot,
        request.slot,
        request.interface.into(),
        request.interpret_unqualified,
    )
    .map_or_else(
        |error| AssessmentEvidence::Failed {
            error: error.to_string(),
        },
        AssessmentEvidence::from,
    );
    SnapshotEvidence {
        path,
        identity: (&snapshot.identity).into(),
        assessment,
    }
}

/// Reject bad sources without displaying strings taken from their contents.
fn load_snapshot(path: &Path, side: &str) -> AppResult<Snapshot> {
    Snapshot::load(path).map_err(|error| {
        let detail = match (
            error.downcast_ref::<std::io::Error>(),
            error.downcast_ref::<serde_json::Error>(),
        ) {
            (Some(error), _) => format!("file access failed ({:?})", error.kind()),
            (_, Some(error)) => format!(
                "invalid report data at line {}, column {} ({:?})",
                error.line(), error.column(), error.classify()
            ),
            _ => "requires a regular, size-bounded, complete successful backup with valid identity and coverage".to_owned(),
        };
        CommandError(format!("{side} report rejected: {detail}. Captured values are not displayed.")).into()
    })
}

fn build_report(request: &CompareRequest) -> AppResult<ComparisonReport> {
    let before = load_snapshot(&request.before, "Before")?;
    let after = load_snapshot(&request.after, "After")?;
    let diff = StandardConfigurationDiff::between(
        &before.standard_configuration()?,
        &after.standard_configuration()?,
    ).map_err(|_| CommandError(
        "Configuration comparison requires identical model, exact firmware, and full radio-type identities with canonical coverage; captured values are not displayed.".to_owned()
    ))?;
    let qualification = if request.interpret_unqualified {
        TextLayoutQualification::UnqualifiedInterpretation
    } else if is_supported_schema_target(before.identity.model, &before.identity.firmware) {
        TextLayoutQualification::RegistryTargetMatched
    } else {
        return Err(CommandError(
            "This firmware requires --interpret-unqualified for offline layout annotations; no radio settings will be changed.".to_owned()
        ).into());
    };
    let catalog = Catalog::new()?;
    let mut fields = BTreeMap::<u32, FieldChange>::new();
    let mut outside_terminal_catalog_bytes = 0;
    let pages = diff
        .pages()
        .iter()
        .map(|page| PageChange {
            address: page.page().address().as_u32(),
            length: page.page().len(),
            changes: page
                .changes()
                .iter()
                .map(|change| {
                    if let Some(field) = catalog.classify(change.address()) {
                        fields
                            .entry(field.start)
                            .or_insert_with(|| FieldChange {
                                field: field.clone(),
                                changed_bytes: 0,
                            })
                            .changed_bytes += 1;
                    } else {
                        outside_terminal_catalog_bytes += 1;
                    }
                    ByteChange {
                        address: change.address().as_u32(),
                        before: change.before(),
                        after: change.after(),
                    }
                })
                .collect(),
        })
        .collect();
    Ok(ComparisonReport {
        format_version: 1,
        operation: "terminal_configuration_comparison",
        software_version: env!("CARGO_PKG_VERSION"),
        qualification: match qualification {
            TextLayoutQualification::RegistryTargetMatched => "registry_target_matched",
            TextLayoutQualification::UnqualifiedInterpretation => "unqualified_interpretation",
        },
        requested_slot: request.slot.index(),
        requested_interface: TerminalUsbRoute::from(request.interface).to_string(),
        before: snapshot_evidence(request.before.clone(), &before, request),
        after: snapshot_evidence(request.after.clone(), &after, request),
        compared_pages: diff.compared_pages(),
        compared_bytes: diff.compared_bytes(),
        changed_bytes: diff.changed_bytes(),
        outside_terminal_catalog_bytes,
        pages,
        fields: fields.into_values().collect(),
        radio_accessed: false,
        radio_applied: false,
        limitations: [
            "Historical reports only; before/after order is caller supplied, not proof of chronology.",
            "Matching public identities do not prove physical-unit continuity or report authenticity.",
            "Structural report validation does not independently authenticate raw transcripts.",
            "Catalog labels and successful decoding do not qualify live settings, PM isolation, activation, or restoration.",
        ],
    })
}

fn describe(report: &ComparisonReport) -> Vec<String> {
    let mut lines = vec![
        "Offline configuration comparison. Historical reports only; no radio access or settings changes.".to_owned(),
        format!("Layout interpretation: {}. This does not qualify live settings.", report.qualification),
        format!("Compared {} pages / {} captured bytes across every PM slot; {} bytes differ in {} pages.", report.compared_pages, report.compared_bytes, report.changed_bytes, report.pages.len()),
        format!("{} changed bytes are outside the finite Terminal catalog; they remain in the exact comparison.", report.outside_terminal_catalog_bytes),
    ];
    for (label, snapshot) in [("Before", &report.before), ("After", &report.after)] {
        let status = match &snapshot.assessment {
            AssessmentEvidence::Decoded { findings, .. } => {
                format!("decoded with {} findings; not a live-readiness result", findings.len())
            }
            AssessmentEvidence::Failed { .. } => {
                "interpretation failed; raw differences retained, details require private JSON output".to_owned()
            }
        };
        lines.push(format!("{label} selected-slot preflight: {status}."));
    }
    for field in &report.fields {
        let scope = field
            .field
            .slot
            .map_or_else(|| "global".to_owned(), |slot| format!("PM slot {slot}"));
        lines.push(format!(
            "Candidate field {} ({scope}): {} changed bytes at field address {}.",
            field.field.key, field.changed_bytes, field.field.start
        ));
    }
    for page in &report.pages {
        lines.push(format!(
            "Changed page {} ({} captured bytes): {} differing bytes.",
            page.address,
            page.length,
            page.changes.len()
        ));
    }
    lines.push("Console omits captured text and byte values. Use --output for a new private JSON report; keep that report private.".to_owned());
    lines.extend(report.limitations.map(str::to_owned));
    lines
}

pub(super) fn run(request: &CompareRequest) -> AppResult<()> {
    let report = build_report(request)?;
    if let Some(path) = &request.output {
        let mut file = capture::create_private_file(path)?;
        write_report(&mut file, &report)?;
        file.sync_all()?;
    }
    for line in describe(&report) {
        output::line(format_args!("{line}"));
    }
    if request.output.is_some() {
        output::line(format_args!(
            "Private comparison report saved. It is not a configuration backup or a write plan."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
