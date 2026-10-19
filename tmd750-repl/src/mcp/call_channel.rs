//! One CALL channel's receive frequency: an offline preview, or one record
//! write over one MCP session.
//!
//! Both start from the backup's record of the chosen CALL channel and keep
//! every other field. `set` writes the record's page through the shared page
//! workflow, which compares the whole page before the write and reads it
//! back. In CALL tuning mode `FO` reports the stored record.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kenwood_tmd750::PageReplacement;
use kenwood_tmd750::memory::{CallBand, CallChannel, CallChannelPlan, CallMode, ChannelAccess};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::{Frequency, StoredChannel};
use serde_json::{Map, Value, json};

use super::channels::covered;
use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// CALL channel subcommand: an offline preview, or one guarded live write.
#[derive(Debug, Parser)]
pub(crate) struct CallChannelRequest {
    #[command(subcommand)]
    command: CallChannelCommand,
}

#[derive(Debug, Subcommand)]
enum CallChannelCommand {
    /// Show one CALL record before and after an edit; never writes.
    Preview(EditRequest),
    /// Write one edited CALL record, comparing and reading back its page.
    Set(SetRequest),
}

#[derive(Debug, Args)]
struct EditRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// The CALL channel to edit.
    #[arg(long, value_enum)]
    channel: Channel,
    /// New receive frequency in MHz, inside the channel's band: 136 to 174
    /// (VHF), 216 to 260 (220 MHz) or 410 to 470 (UHF), upper bound excluded.
    #[arg(long, value_name = "MHZ")]
    frequency: String,
}

/// Arguments of `mcp call-channel set`.
#[derive(Debug, Args)]
pub(super) struct SetRequest {
    #[command(flatten)]
    edit: EditRequest,
    /// Required. The edited record stays on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// A CALL channel as the command line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Channel {
    /// The VHF FM CALL channel.
    #[value(name = "vhf-fm")]
    VhfFm,
    /// The VHF DV CALL channel.
    #[value(name = "vhf-dv")]
    VhfDv,
    /// The 220 MHz FM CALL channel.
    #[value(name = "220-fm")]
    Band220Fm,
    /// The 220 MHz DV CALL channel.
    #[value(name = "220-dv")]
    Band220Dv,
    /// The UHF FM CALL channel.
    #[value(name = "uhf-fm")]
    UhfFm,
    /// The UHF DV CALL channel.
    #[value(name = "uhf-dv")]
    UhfDv,
}

impl Channel {
    const fn call_channel(self) -> CallChannel {
        let (band, mode) = match self {
            Self::VhfFm => (CallBand::Vhf, CallMode::Fm),
            Self::VhfDv => (CallBand::Vhf, CallMode::Dv),
            Self::Band220Fm => (CallBand::Band220, CallMode::Fm),
            Self::Band220Dv => (CallBand::Band220, CallMode::Dv),
            Self::UhfFm => (CallBand::Uhf, CallMode::Fm),
            Self::UhfDv => (CallBand::Uhf, CallMode::Dv),
        };
        CallChannel::new(band, mode)
    }
}

/// The journal spelling of a CALL channel, as the command line names it.
const fn label(channel: CallChannel) -> &'static str {
    match (channel.band(), channel.mode()) {
        (CallBand::Vhf, CallMode::Fm) => "vhf-fm",
        (CallBand::Vhf, CallMode::Dv) => "vhf-dv",
        (CallBand::Band220, CallMode::Fm) => "220-fm",
        (CallBand::Band220, CallMode::Dv) => "220-dv",
        (CallBand::Uhf, CallMode::Fm) => "uhf-fm",
        (CallBand::Uhf, CallMode::Dv) => "uhf-dv",
    }
}

/// One planned edit: the channel, its record before and after, and the page.
struct Prepared {
    channel: CallChannel,
    before: StoredChannel,
    after: StoredChannel,
    replacement: Option<PageReplacement>,
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when the backup lacks the channel tables or the
    /// channel's record, or the frequency is outside the channel's band.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let channel = self.channel.call_channel();
        let image = covered(snapshot)?;
        let entry = ChannelAccess::from_memory_image(image).call_channel(channel)?;
        let before = entry
            .record()
            .cloned()
            .ok_or_else(|| CommandError(format!("the backup holds no {channel} record")))?;
        let mut after = before.clone();
        after.receive_frequency = Frequency::from_mhz_str(&self.frequency)?;
        let plan = CallChannelPlan::new([(channel, after.clone())])?;
        let replacement = plan.page_replacements(image)?.into_iter().next();
        Ok(Prepared {
            channel,
            before,
            after,
            replacement,
        })
    }
}

impl CallChannelRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, CallChannelCommand::Set(_)) && !explicit_port {
            return Err(CommandError(
                "mcp call-channel set requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &CallChannelRequest) -> Option<AppResult<()>> {
    match &request.command {
        CallChannelCommand::Preview(edit) => Some(preview(edit)),
        CallChannelCommand::Set(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &CallChannelRequest,
) -> AppResult<()> {
    match &request.command {
        CallChannelCommand::Set(request) => run_set(endpoint, baud, request).await,
        CallChannelCommand::Preview(edit) => preview(edit),
    }
}

async fn run_set(endpoint: &SerialCandidate, baud: u32, request: &SetRequest) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::CallChannel)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    print_edit(&prepared);
    let Some(replacement) = &prepared.replacement else {
        return Err(CommandError(format!(
            "the backup's {} record already holds {}; nothing was opened",
            prepared.channel, prepared.after.receive_frequency
        ))
        .into());
    };
    let plan = PagePlan::pages(
        PlanKind::CallChannel,
        snapshot.identity(),
        std::slice::from_ref(replacement),
        description(&prepared),
    );
    page_apply::run_usb(
        endpoint,
        baud,
        &plan,
        request.output.as_deref(),
        &request.edit.backup,
    )
    .await
}

fn preview(edit: &EditRequest) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_edit(&prepared);
    match &prepared.replacement {
        Some(replacement) => output::line(format_args!(
            "Preview only; mcp call-channel set --apply writes page {:#07X}.",
            replacement.page().address().as_u32()
        )),
        None => output::line(format_args!(
            "The backup's record already holds this frequency; set would write nothing."
        )),
    }
    Ok(())
}

fn print_edit(prepared: &Prepared) {
    output::line(format_args!(
        "{} before: {} {}",
        prepared.channel, prepared.before.receive_frequency, prepared.before.mode
    ));
    output::line(format_args!(
        "{} after:  {} {}",
        prepared.channel, prepared.after.receive_frequency, prepared.after.mode
    ));
}

/// The journal description of an edit: the channel and both receive
/// frequencies.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let mut description = Map::new();
    let _previous = description.insert(
        "call_channel".to_owned(),
        json!({
            "channel": label(prepared.channel),
            "before_hz": prepared.before.receive_frequency.as_hz(),
            "after_hz": prepared.after.receive_frequency.as_hz(),
        }),
    );
    description
}

#[cfg(test)]
mod tests;
