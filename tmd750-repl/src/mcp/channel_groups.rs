//! Memory-channel groups: an offline preview of the flag-table pages a set of
//! group assignments changes, and one write of those pages over one MCP
//! session.
//!
//! The pages are planned against the source backup and written through the
//! shared page workflow, which compares each whole page before its write and
//! reads it back. Each channel must be stored, and a CAT store sets its group
//! to the channel number's hundreds digit, so assign groups after storing.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{ChannelAccess, ChannelGroupPlan, MemoryGroup};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::PhysicalChannel;
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::cat::parse_address;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
mod tests;

/// Channel-group subcommand: an offline preview, or one guarded live write.
#[derive(Debug, Parser)]
pub(crate) struct ChannelGroupsRequest {
    #[command(subcommand)]
    command: ChannelGroupsCommand,
}

#[derive(Debug, Subcommand)]
enum ChannelGroupsCommand {
    /// Show the groups a set of assignments changes; never writes.
    Preview(GroupSelection),
    /// Write the groups in one session, comparing and reading back each page.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct GroupSelection {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Put stored CHANNEL (000-999) in GROUP (0 to 29); repeatable.
    #[arg(
        long = "set",
        num_args = 2,
        value_names = ["CHANNEL", "GROUP"],
        action = clap::ArgAction::Append
    )]
    set: Vec<String>,
}

/// Arguments of `mcp channel-groups apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    groups: GroupSelection,
    /// Required. The groups stay on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// One channel's group before and after the edit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    channel: PhysicalChannel,
    before: u8,
    after: MemoryGroup,
}

/// A planned set of groups: each channel's change and the pages that change.
struct Prepared {
    changes: Vec<Change>,
    replacements: Vec<PageReplacement>,
}

/// The flag table of a backup that captured it.
fn captured(snapshot: &Snapshot) -> AppResult<&kenwood_tmd750::MemoryImage> {
    let [flags, _, _] = ChannelAccess::regions()?;
    snapshot.image_covering(Region::new(flags.start(), flags.end())?)
}

impl GroupSelection {
    /// Plan the groups against the backup, before any capture or radio work.
    ///
    /// Returns an error when no channel is given, a channel or group is
    /// invalid or not a stored regular channel, or a channel appears twice.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let image = captured(snapshot)?;
        let access = ChannelAccess::from_memory_image(image);
        let mut assignments = Vec::new();
        let mut changes = Vec::new();
        for pair in self.set.chunks_exact(2) {
            let [channel, group] = pair else {
                continue;
            };
            let channel = PhysicalChannel::from_address(parse_address(channel)?);
            let group = group
                .parse()
                .map_err(|_| CommandError(format!("group {group:?} is not 0 to 29")))
                .and_then(|index| {
                    MemoryGroup::new(index).map_err(|error| CommandError(error.to_string()))
                })?;
            assignments.push((channel, group));
            changes.push(Change {
                channel,
                before: access.entry(channel)?.flag().group(),
                after: group,
            });
        }
        if assignments.is_empty() {
            return Err(CommandError("give at least one --set CHANNEL GROUP".to_owned()).into());
        }
        let replacements = ChannelGroupPlan::new(assignments)?.page_replacements(image)?;
        Ok(Prepared {
            changes,
            replacements,
        })
    }
}

impl ChannelGroupsRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, ChannelGroupsCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp channel-groups apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &ChannelGroupsRequest) -> Option<AppResult<()>> {
    match &request.command {
        ChannelGroupsCommand::Preview(groups) => Some(preview(groups)),
        ChannelGroupsCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ChannelGroupsRequest,
) -> AppResult<()> {
    match &request.command {
        ChannelGroupsCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        ChannelGroupsCommand::Preview(groups) => preview(groups),
    }
}

async fn run_apply(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ApplyRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::ChannelGroups)?;
    let snapshot = Snapshot::load_for_usb_write(&request.groups.backup)?;
    let prepared = request.groups.prepare(&snapshot)?;
    print_changes(&prepared);
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already holds these groups; nothing was opened".to_owned(),
        )
        .into());
    }
    let plan = PagePlan::pages(
        PlanKind::ChannelGroups,
        snapshot.identity(),
        &prepared.replacements,
        description(&prepared),
    );
    page_apply::run_usb(
        endpoint,
        baud,
        &plan,
        request.output.as_deref(),
        &request.groups.backup,
    )
    .await
}

fn preview(groups: &GroupSelection) -> AppResult<()> {
    let prepared = groups.prepare(&Snapshot::load(&groups.backup)?)?;
    print_changes(&prepared);
    if prepared.replacements.is_empty() {
        output::line(format_args!(
            "The backup already holds these groups; apply would write nothing."
        ));
    } else {
        let pages: Vec<String> = prepared
            .replacements
            .iter()
            .map(|replacement| format!("{:#07X}", replacement.page().address().as_u32()))
            .collect();
        output::line(format_args!(
            "Preview only; mcp channel-groups apply --apply writes pages {}.",
            pages.join(", ")
        ));
    }
    Ok(())
}

fn print_changes(prepared: &Prepared) {
    for change in &prepared.changes {
        output::line(format_args!(
            "Channel {}: group {} -> {}.",
            change.channel,
            change.before,
            change.after.index()
        ));
    }
}

/// The journal description of an edit: each channel's group before and
/// after.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let changes: Vec<Value> = prepared
        .changes
        .iter()
        .map(|change| {
            json!({
                "channel": change.channel.to_string(),
                "before": change.before,
                "after": change.after.index(),
            })
        })
        .collect();
    let mut description = Map::new();
    let _previous = description.insert("channel_groups".to_owned(), Value::Array(changes));
    description
}
