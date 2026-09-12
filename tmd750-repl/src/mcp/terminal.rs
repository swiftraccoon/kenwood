//! Historical Terminal-settings assessment, with no endpoint or write access.

mod compare;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kenwood_tmd750::SlotIndex;
use kenwood_tmd750::memory::{
    ReflectorTerminalPreflight, TerminalUsbRoute, TextLayoutQualification,
};

use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// Inspect Terminal configuration from a completed local backup only.
#[derive(Debug, Parser)]
pub(crate) struct TerminalRequest {
    #[command(subcommand)]
    command: TerminalCommand,
}

#[derive(Debug, Subcommand)]
enum TerminalCommand {
    /// Assess historical Reflector Terminal settings; never test live readiness.
    Preflight(PreflightRequest),
    /// Compare all captured bytes and annotate candidate Terminal settings.
    Compare(compare::CompareRequest),
}

#[derive(Debug, Args)]
struct PreflightRequest {
    /// Successful format-3 configuration report, not a raw image or partial probe.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,

    /// Explicit programmable-memory slot: 0 is PM Off, 1 through 5 are PM 1–5.
    #[arg(long, value_parser = parse_slot, value_name = "0..5")]
    slot: SlotIndex,

    /// Intended USB connection; this does not discover or select a live endpoint.
    #[arg(long, value_enum)]
    interface: UsbInterface,

    /// Explicitly interpret firmware outside the registry label, without writes.
    ///
    /// Required for firmware 1.02; successful decoding does not qualify the layout.
    #[arg(long)]
    interpret_unqualified: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum UsbInterface {
    MainUsb,
    PanelUsb,
}

impl From<UsbInterface> for TerminalUsbRoute {
    fn from(interface: UsbInterface) -> Self {
        match interface {
            UsbInterface::MainUsb => Self::MainUnit,
            UsbInterface::PanelUsb => Self::ControlPanel,
        }
    }
}

fn parse_slot(value: &str) -> Result<SlotIndex, String> {
    let index = value
        .parse::<u8>()
        .map_err(|_| "slot must be an integer from 0 through 5".to_owned())?;
    SlotIndex::new(index).map_err(|error| error.to_string())
}

fn assess(
    request: &PreflightRequest,
    snapshot: &Snapshot,
) -> AppResult<ReflectorTerminalPreflight> {
    assess_snapshot(
        snapshot,
        request.slot,
        request.interface.into(),
        request.interpret_unqualified,
    )
}

fn assess_snapshot(
    snapshot: &Snapshot,
    slot: SlotIndex,
    route: TerminalUsbRoute,
    interpret_unqualified: bool,
) -> AppResult<ReflectorTerminalPreflight> {
    // Check every dependency before exposing the internal buffer to the view.
    // Unread gaps in that buffer are placeholders, not captured settings.
    let mut covered_image = None;
    for field in ReflectorTerminalPreflight::required_fields()? {
        covered_image = Some(snapshot.image_for(field, Some(slot))?);
    }
    let image = covered_image
        .ok_or_else(|| CommandError("Terminal preflight has no storage descriptors".to_owned()))?;
    Ok(if interpret_unqualified {
        ReflectorTerminalPreflight::interpret_unqualified(
            image,
            &snapshot.identity.firmware,
            slot,
            route,
        )?
    } else {
        ReflectorTerminalPreflight::read(image, &snapshot.identity.firmware, slot, route)?
    })
}

/// Run before endpoint enumeration; accepts no transport or connected radio.
pub(super) fn run(request: &TerminalRequest) -> AppResult<()> {
    match &request.command {
        TerminalCommand::Preflight(request) => {
            let snapshot = Snapshot::load(&request.backup)?;
            let assessment = assess(request, &snapshot)?;
            for line in describe(&snapshot, &assessment)? {
                output::line(format_args!("{line}"));
            }
            Ok(())
        }
        TerminalCommand::Compare(request) => compare::run(request),
    }
}

fn describe(
    snapshot: &Snapshot,
    assessment: &ReflectorTerminalPreflight,
) -> AppResult<Vec<String>> {
    let qualification = match assessment.qualification {
        TextLayoutQualification::RegistryTargetMatched => {
            "Registry firmware label matched; hardware layout compatibility is not proved."
        }
        TextLayoutQualification::UnqualifiedInterpretation => {
            "Unqualified software-layout interpretation; these are not validated radio settings."
        }
    };
    let mut lines = vec![
        "Offline Reflector Terminal preflight. Historical backup only; current radio state is unknown."
            .to_owned(),
        format!(
            "Backup identity: {}; firmware {}; type {}.",
            snapshot.identity.model, snapshot.identity.firmware, snapshot.identity.radio_type
        ),
        qualification.to_owned(),
        format!(
            "Requested PM slot: {}; captured active PM slot: {}. Slot 0 means PM Off.",
            assessment.slot.index(),
            assessment.captured_active_slot.index()
        ),
        format!(
            "Requested connection: {}; captured gateway route: {}.",
            assessment.desired_route, assessment.gateway_route
        ),
        format!(
            "Menu 980 USB function: {}; Menu 650 gateway mode: {}; Menu 670 Terminal subtype: {}.",
            assessment.usb_function, assessment.gateway_mode, assessment.terminal_mode
        ),
        format!(
            "Menu 651 selected MY entry: {}; callsign: {}.",
            assessment.selected_my_callsign.index() + 1,
            serde_json::to_string(&assessment.my_callsign)?
        ),
        format!(
            "Menu 671 RPT1: {}; Menu 672 RPT2: {}.",
            serde_json::to_string(&assessment.rpt1)?,
            serde_json::to_string(&assessment.rpt2)?
        ),
    ];
    if assessment.findings.is_empty() {
        lines.push(
            "No conflicts found by these offline checks. This is not a live-readiness result."
                .to_owned(),
        );
    } else {
        for finding in &assessment.findings {
            lines.push(format!("Finding: {finding}"));
        }
    }
    lines.push(
        concat!(
            "Active DV Gateway rejects CAT on its assigned interface. ",
            "Main-unit ID/FV/TY/GW queries were observed on firmware 1.02 ",
            "with Terminal selected and the Gateway routed to panel USB. ",
            "This does not establish current routing, general CAT access, ",
            "MCP readiness, or qualified automatic exit."
        )
        .to_owned(),
    );
    lines.push(
        "No radio endpoints enumerated or opened. No PM recalled, text normalized, patch generated, or settings changed. Live activation and restoration remain unqualified."
            .to_owned(),
    );
    Ok(lines)
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
