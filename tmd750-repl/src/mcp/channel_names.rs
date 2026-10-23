//! Memory-channel names: an offline preview of the name-table pages a set of
//! names changes, and one write of those pages over one MCP session.
//!
//! The pages are planned against the source backup and written through the
//! shared page workflow, which compares each whole page before its write, so
//! a name changed on the radio since the backup stops the run before any
//! write. Store the channel records themselves over CAT first: a CAT store
//! keeps a channel's name and a CAT clear erases it.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{ChannelNamePlan, ChannelNameText};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::{
    CHANNEL_NAME_SIZE, CHANNEL_NAMES_OFFSET, PHYSICAL_CHANNEL_COUNT, PhysicalChannel,
};
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::cat::parse_address;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
mod tests;

/// Channel-name subcommand: an offline preview, or one guarded live write.
#[derive(Debug, Parser)]
pub(crate) struct ChannelNamesRequest {
    #[command(subcommand)]
    command: ChannelNamesCommand,
}

#[derive(Debug, Subcommand)]
enum ChannelNamesCommand {
    /// Show the name-table pages a set of names changes; never writes.
    Preview(NamesSelection),
    /// Write the names in one session, comparing and reading back each page.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct NamesSelection {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Name CHANNEL (000-999, L00-L49, U00-U49 or Pri) NAME, at most sixteen
    /// printable ASCII bytes; repeatable.
    #[arg(
        long = "set",
        num_args = 2,
        value_names = ["CHANNEL", "NAME"],
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    set: Vec<String>,
    /// Leave CHANNEL unnamed; repeatable.
    #[arg(long = "clear", value_name = "CHANNEL", action = clap::ArgAction::Append)]
    clear: Vec<String>,
}

/// Arguments of `mcp channel-names apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    names: NamesSelection,
    /// Required. The names stay on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// A planned set of names: each channel's wire address and name, and the
/// pages that change.
struct Prepared {
    entries: Vec<(String, Option<String>)>,
    replacements: Vec<PageReplacement>,
}

/// The image span of the channel name table.
fn name_table() -> AppResult<Region> {
    let start = u32::try_from(CHANNEL_NAMES_OFFSET)?;
    let length = u32::try_from(CHANNEL_NAME_SIZE)? * u32::from(PHYSICAL_CHANNEL_COUNT);
    Ok(Region::new(start, start + length)?)
}

impl NamesSelection {
    /// Plan the names against the backup, before any capture or radio work.
    ///
    /// Returns an error when no channel is given, a channel or name is
    /// invalid, or a channel appears twice.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let mut names = Vec::new();
        let mut entries = Vec::new();
        for [channel, name] in self.set.as_chunks::<2>().0 {
            let address = parse_address(channel)?;
            names.push((
                PhysicalChannel::from_address(address),
                Some(ChannelNameText::new(name)?),
            ));
            entries.push((address.to_string(), Some(name.clone())));
        }
        for channel in &self.clear {
            let address = parse_address(channel)?;
            names.push((PhysicalChannel::from_address(address), None));
            entries.push((address.to_string(), None));
        }
        if names.is_empty() {
            return Err(CommandError(
                "give at least one --set CHANNEL NAME or --clear CHANNEL".to_owned(),
            )
            .into());
        }
        let plan = ChannelNamePlan::new(names)?;
        let replacements = plan.page_replacements(snapshot.image_covering(name_table()?)?)?;
        Ok(Prepared {
            entries,
            replacements,
        })
    }
}

impl ChannelNamesRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, ChannelNamesCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp channel-names apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &ChannelNamesRequest) -> Option<AppResult<()>> {
    match &request.command {
        ChannelNamesCommand::Preview(names) => Some(preview(names)),
        ChannelNamesCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ChannelNamesRequest,
) -> AppResult<()> {
    match &request.command {
        ChannelNamesCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        ChannelNamesCommand::Preview(names) => preview(names),
    }
}

async fn run_apply(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ApplyRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::ChannelNames)?;
    let snapshot = Snapshot::load_for_usb_write(&request.names.backup)?;
    let prepared = request.names.prepare(&snapshot)?;
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already contains every requested channel name; nothing was opened"
                .to_owned(),
        )
        .into());
    }
    print_plan(&prepared);
    let plan = PagePlan::pages(
        PlanKind::ChannelNames,
        snapshot.identity(),
        &prepared.replacements,
        description(&prepared),
    );
    page_apply::run_usb(
        endpoint,
        baud,
        &plan,
        request.output.as_deref(),
        &request.names.backup,
    )
    .await
}

fn preview(names: &NamesSelection) -> AppResult<()> {
    let prepared = names.prepare(&Snapshot::load(&names.backup)?)?;
    print_plan(&prepared);
    output::line(format_args!(
        "Preview only; mcp channel-names apply --apply writes {} pages.",
        prepared.replacements.len()
    ));
    Ok(())
}

fn print_plan(prepared: &Prepared) {
    for (channel, name) in &prepared.entries {
        match name {
            Some(name) => output::line(format_args!("Channel {channel}: {name:?}.")),
            None => output::line(format_args!("Channel {channel}: unnamed.")),
        }
    }
    for replacement in &prepared.replacements {
        output::line(format_args!(
            "Name-table page {:#07X} changes.",
            replacement.page().address().as_u32()
        ));
    }
}

/// The journal description of a plan: each channel and its name.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let mut description = Map::new();
    let _previous = description.insert(
        "channel_names".to_owned(),
        prepared
            .entries
            .iter()
            .map(|(channel, name)| json!({ "channel": channel, "name": name }))
            .collect(),
    );
    description
}
