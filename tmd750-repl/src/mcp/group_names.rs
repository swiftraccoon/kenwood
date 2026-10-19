//! Memory group names: an offline listing and preview, and one write of the
//! changed name-table pages over one MCP session.
//!
//! The pages are planned against the source backup and written through the
//! shared page workflow, which compares each whole page before its write and
//! reads it back.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{ChannelAccess, ChannelNameText, GroupNamePlan, MemoryGroup};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// Group-name subcommand: an offline listing or preview, or one guarded live
/// write.
#[derive(Debug, Parser)]
pub(crate) struct GroupNamesRequest {
    #[command(subcommand)]
    command: GroupNamesCommand,
}

#[derive(Debug, Subcommand)]
enum GroupNamesCommand {
    /// Print the backup's group names and the weather group's name; never
    /// writes.
    List(ListRequest),
    /// Show the names a set of edits changes; never writes.
    Preview(NamesSelection),
    /// Write the names in one session, comparing and reading back each page.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
}

#[derive(Debug, Args)]
struct NamesSelection {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Name GROUP (0 to 29) NAME, 1 to 16 printable ASCII bytes; repeatable.
    #[arg(
        long = "set",
        num_args = 2,
        value_names = ["GROUP", "NAME"],
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    set: Vec<String>,
}

/// Arguments of `mcp group-names apply`.
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

/// One group's name before and after the edit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    group: MemoryGroup,
    before: String,
    after: String,
}

/// A planned set of names: each group's change and the pages that change.
struct Prepared {
    changes: Vec<Change>,
    replacements: Vec<PageReplacement>,
}

/// The names decoded from a backup that captured every group name.
fn captured(snapshot: &Snapshot) -> AppResult<&kenwood_tmd750::MemoryImage> {
    let start = u32::try_from(MemoryGroup::new(0)?.name_address())?;
    let end = u32::try_from(ChannelAccess::REQUIRED_LENGTH)?;
    snapshot.image_covering(Region::new(start, end)?)
}

impl NamesSelection {
    /// Plan the names against the backup, before any capture or radio work.
    ///
    /// Returns an error when no group is given, a group or name is invalid,
    /// or a group appears twice.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let image = captured(snapshot)?;
        let access = ChannelAccess::from_memory_image(image);
        let mut names = Vec::new();
        let mut changes = Vec::new();
        for pair in self.set.chunks_exact(2) {
            let [group, name] = pair else {
                continue;
            };
            let group = group
                .parse()
                .map_err(|_| CommandError(format!("group {group:?} is not 0 to 29")))
                .and_then(|index| {
                    MemoryGroup::new(index).map_err(|error| CommandError(error.to_string()))
                })?;
            names.push((group, ChannelNameText::new(name)?));
            changes.push(Change {
                group,
                before: access.group_name(group).text(),
                after: name.clone(),
            });
        }
        if names.is_empty() {
            return Err(CommandError("give at least one --set GROUP NAME".to_owned()).into());
        }
        let replacements = GroupNamePlan::new(names)?.page_replacements(image)?;
        Ok(Prepared {
            changes,
            replacements,
        })
    }
}

impl GroupNamesRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, GroupNamesCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp group-names apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &GroupNamesRequest) -> Option<AppResult<()>> {
    match &request.command {
        GroupNamesCommand::List(list) => Some(print_list(list)),
        GroupNamesCommand::Preview(names) => Some(preview(names)),
        GroupNamesCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &GroupNamesRequest,
) -> AppResult<()> {
    match &request.command {
        GroupNamesCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        GroupNamesCommand::List(list) => print_list(list),
        GroupNamesCommand::Preview(names) => preview(names),
    }
}

async fn run_apply(endpoint: &SerialCandidate, baud: u32, request: &ApplyRequest) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::GroupNames)?;
    let snapshot = Snapshot::load_for_usb_write(&request.names.backup)?;
    let prepared = request.names.prepare(&snapshot)?;
    print_changes(&prepared);
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already holds these names; nothing was opened".to_owned(),
        )
        .into());
    }
    let plan = PagePlan::pages(
        PlanKind::GroupNames,
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

fn print_list(request: &ListRequest) -> AppResult<()> {
    let snapshot = Snapshot::load(&request.backup)?;
    let access = ChannelAccess::from_memory_image(captured(&snapshot)?);
    for (group, name) in access.group_names() {
        output::line(format_args!("Group {}: {:?}", group.index(), name.text()));
    }
    output::line(format_args!(
        "Weather group: {:?}",
        access.weather_group_name().text()
    ));
    Ok(())
}

fn preview(names: &NamesSelection) -> AppResult<()> {
    let prepared = names.prepare(&Snapshot::load(&names.backup)?)?;
    print_changes(&prepared);
    if prepared.replacements.is_empty() {
        output::line(format_args!(
            "The backup already holds these names; apply would write nothing."
        ));
    } else {
        let pages: Vec<String> = prepared
            .replacements
            .iter()
            .map(|replacement| format!("{:#07X}", replacement.page().address().as_u32()))
            .collect();
        output::line(format_args!(
            "Preview only; mcp group-names apply --apply writes pages {}.",
            pages.join(", ")
        ));
    }
    Ok(())
}

fn print_changes(prepared: &Prepared) {
    for change in &prepared.changes {
        output::line(format_args!(
            "Group {}: {:?} -> {:?}",
            change.group.index(),
            change.before,
            change.after
        ));
    }
}

/// The journal description of an edit: each group's name before and after.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let changes: Vec<Value> = prepared
        .changes
        .iter()
        .map(|change| {
            json!({
                "group": change.group.index(),
                "before": change.before,
                "after": change.after,
            })
        })
        .collect();
    let mut description = Map::new();
    let _previous = description.insert("group_names".to_owned(), Value::Array(changes));
    description
}

#[cfg(test)]
mod tests;
