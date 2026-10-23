//! The D-STAR callsign list: an offline listing and preview, and one write
//! of the edited list over one MCP session.
//!
//! An edit starts from the backup's list in display order: `--remove` drops
//! the entries at the given positions and `--add` appends entries. The
//! result is stored compact from slot 0 through the shared page workflow,
//! which compares each whole page before its write, so a list changed on the
//! radio since the backup stops the run before any write.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::StoredText;
use kenwood_tmd750::memory::callsign_list::{
    CALLSIGN_ENTRY_SIZE, CALLSIGN_LIST_ADDRESS, CALLSIGN_LIST_CAPACITY, CALLSIGN_LIST_ENDS_ADDRESS,
    CallsignEntry, CallsignList,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::{PAGE_SIZE_U32, StoredCallsign};
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
mod tests;

/// Callsign-list subcommand: an offline listing or preview, or one guarded
/// live write.
#[derive(Debug, Parser)]
pub(crate) struct CallsignListRequest {
    #[command(subcommand)]
    command: CallsignListCommand,
}

#[derive(Debug, Subcommand)]
enum CallsignListCommand {
    /// Print the backup's entries in display order; never writes.
    List(ListRequest),
    /// Show the edited list and the pages it changes; never writes.
    Preview(EditRequest),
    /// Write the edited list in one session, comparing and reading back each
    /// page.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
}

#[derive(Debug, Args)]
struct EditRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Remove the entry at display POSITION, counted from 1 as `list` numbers
    /// them; repeatable.
    #[arg(long = "remove", value_name = "POSITION", action = clap::ArgAction::Append)]
    remove: Vec<usize>,
    /// Append an entry: NAME of at most sixteen stored bytes, CALLSIGN of at
    /// most eight graphic ASCII bytes or spaces, MEMO of at most 32 stored
    /// bytes ("" for none); repeatable.
    #[arg(
        long = "add",
        num_args = 3,
        value_names = ["NAME", "CALLSIGN", "MEMO"],
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    add: Vec<String>,
}

/// Arguments of `mcp callsign-list apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    edit: EditRequest,
    /// Required. The list stays on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// A planned edit: the list before and after, and the pages that change.
struct Prepared {
    before: CallsignList,
    after: CallsignList,
    replacements: Vec<PageReplacement>,
}

/// The image span of the list's slots.
fn table() -> AppResult<Region> {
    let length = u32::from(CALLSIGN_LIST_CAPACITY) * u32::try_from(CALLSIGN_ENTRY_SIZE)?;
    Ok(Region::new(
        CALLSIGN_LIST_ADDRESS,
        CALLSIGN_LIST_ADDRESS + length,
    )?)
}

/// The image span of the page holding the list's ends.
fn ends_page() -> AppResult<Region> {
    let start = CALLSIGN_LIST_ENDS_ADDRESS - CALLSIGN_LIST_ENDS_ADDRESS % PAGE_SIZE_U32;
    Ok(Region::new(start, start + PAGE_SIZE_U32)?)
}

/// The list decoded from a backup that captured its slots and ends.
fn captured(snapshot: &Snapshot) -> AppResult<(CallsignList, &kenwood_tmd750::MemoryImage)> {
    let _table = snapshot.image_covering(table()?)?;
    let image = snapshot.image_covering(ends_page()?)?;
    Ok((CallsignList::read(image)?, image))
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when no edit is given, a position is outside the list
    /// or repeated, an entry's text is invalid, or the list would exceed its
    /// slots.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        if self.remove.is_empty() && self.add.is_empty() {
            return Err(CommandError(
                "give at least one --add NAME CALLSIGN MEMO or --remove POSITION".to_owned(),
            )
            .into());
        }
        let (before, image) = captured(snapshot)?;
        let mut kept: Vec<Option<CallsignEntry>> = before
            .iter()
            .map(|(_, entry)| Some(entry.clone()))
            .collect();
        for &position in &self.remove {
            let entry = position
                .checked_sub(1)
                .and_then(|index| kept.get_mut(index))
                .ok_or_else(|| {
                    CommandError(format!(
                        "position {position} is not in the backup's list of {} entries",
                        before.len()
                    ))
                })?;
            if entry.take().is_none() {
                return Err(
                    CommandError(format!("position {position} is removed more than once")).into(),
                );
            }
        }
        let mut entries: Vec<CallsignEntry> = kept.into_iter().flatten().collect();
        for [name, callsign, memo] in self.add.as_chunks::<3>().0 {
            entries.push(CallsignEntry::new(
                StoredText::new(name)?,
                StoredCallsign::new(callsign)?,
                StoredText::new(memo)?,
            )?);
        }
        let after = CallsignList::from_entries(entries)?;
        let replacements = after.page_replacements(image)?;
        Ok(Prepared {
            before,
            after,
            replacements,
        })
    }
}

impl CallsignListRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, CallsignListCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp callsign-list apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &CallsignListRequest) -> Option<AppResult<()>> {
    match &request.command {
        CallsignListCommand::List(list) => Some(print_list(list)),
        CallsignListCommand::Preview(edit) => Some(preview(edit)),
        CallsignListCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &CallsignListRequest,
) -> AppResult<()> {
    match &request.command {
        CallsignListCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        CallsignListCommand::List(list) => print_list(list),
        CallsignListCommand::Preview(edit) => preview(edit),
    }
}

async fn run_apply(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ApplyRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::CallsignList)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already contains the edited list; nothing was opened".to_owned(),
        )
        .into());
    }
    print_entries("After", &prepared.after);
    print_pages(&prepared);
    let plan = PagePlan::pages(
        PlanKind::CallsignList,
        snapshot.identity(),
        &prepared.replacements,
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

fn print_list(request: &ListRequest) -> AppResult<()> {
    let (list, _image) = captured(&Snapshot::load(&request.backup)?)?;
    print_entries("Backup", &list);
    Ok(())
}

fn preview(edit: &EditRequest) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_entries("After", &prepared.after);
    print_pages(&prepared);
    output::line(format_args!(
        "Preview only; mcp callsign-list apply --apply writes {} pages.",
        prepared.replacements.len()
    ));
    Ok(())
}

fn print_entries(heading: &str, list: &CallsignList) {
    output::line(format_args!(
        "{heading}: {} of {CALLSIGN_LIST_CAPACITY} entries.",
        list.len()
    ));
    for (position, (slot, entry)) in (1..).zip(list.iter()) {
        output::line(format_args!(
            "{position:>3}. {:?} {:?} memo {:?} (slot {}).",
            entry.name().to_string(),
            entry.callsign().text(),
            entry.memo().to_string(),
            slot.index()
        ));
    }
}

fn print_pages(prepared: &Prepared) {
    for replacement in &prepared.replacements {
        output::line(format_args!(
            "Page {:#07X} changes.",
            replacement.page().address().as_u32()
        ));
    }
}

/// The journal description of a plan: the list before and after.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let entries = |list: &CallsignList| -> Value {
        list.iter()
            .map(|(slot, entry)| {
                json!({
                    "slot": slot.index(),
                    "name": entry.name().to_string(),
                    "callsign": entry.callsign().text(),
                    "memo": entry.memo().to_string(),
                })
            })
            .collect()
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "callsign_list".to_owned(),
        json!({
            "before": entries(&prepared.before),
            "after": entries(&prepared.after),
        }),
    );
    description
}
