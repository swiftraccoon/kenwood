//! Memory channel stores and clears: an offline preview of the flag, record
//! and name pages they change, and one write of those pages over one MCP
//! session.
//!
//! A store takes band A's or B's current VFO record in PM Off, the record
//! `FO` reports, or another stored channel's record and lockout, and writes
//! what a CAT store of that record writes; a clear writes what a CAT clear
//! leaves. The priority channel, which CAT refuses, takes both. The pages are
//! planned against the source backup and written through the shared page
//! workflow, which compares each whole page before its write and reads it
//! back.

use std::path::PathBuf;

use clap::Args;
use kenwood_tmd750::PageReplacement;
use kenwood_tmd750::memory::{ChannelAccess, MemoryChannelPlan};
use kenwood_tmd750::radio::ChannelWrite;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::{
    Band, CatMemoryChannelRecord, MemoryChannelAddress, PhysicalChannel, SlotIndex,
};
use serde_json::{Map, Value, json};

use super::covered;
use crate::cat::parse_address;
use crate::mcp::page_apply::{self, PagePlan, PlanKind};
use crate::mcp::snapshot::Snapshot;
use crate::mcp::vfo;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
mod tests;

/// The stores and clears of `mcp channels preview` and `apply`.
#[derive(Debug, Args)]
pub(super) struct EditSelection {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Store CHANNEL (000-999, L00-L49, U00-U49 or Pri) from SOURCE: a or b
    /// for that band's current VFO record in PM Off, or a stored channel;
    /// repeatable.
    #[arg(
        long = "store",
        num_args = 2,
        value_names = ["CHANNEL", "SOURCE"],
        action = clap::ArgAction::Append
    )]
    store: Vec<String>,
    /// Clear CHANNEL and its name; repeatable.
    #[arg(long = "clear", value_name = "CHANNEL", action = clap::ArgAction::Append)]
    clear: Vec<String>,
}

/// Arguments of `mcp channels apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    edits: EditSelection,
    /// Required. The channels stay on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// One channel's contents before and after the edit, and the CAT fields of
/// a stored record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    address: MemoryChannelAddress,
    before: String,
    after: String,
    record: Option<String>,
}

/// A planned edit: each channel's change and the pages that change.
struct Prepared {
    changes: Vec<Change>,
    replacements: Vec<PageReplacement>,
}

impl EditSelection {
    /// Plan the stores and clears against the backup, before any capture or
    /// radio work.
    ///
    /// Returns an error when no channel is given, a channel or source is
    /// invalid, a source channel is empty, or a channel appears twice.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let image = covered(snapshot)?;
        let access = ChannelAccess::from_memory_image(image);
        let mut writes = Vec::new();
        for pair in self.store.chunks_exact(2) {
            let [channel, source] = pair else {
                continue;
            };
            let address = parse_address(channel)?;
            let record = source_record(snapshot, &access, source)?;
            writes.push((address, ChannelWrite::Store(record)));
        }
        for channel in &self.clear {
            let address = parse_address(channel)?;
            writes.push((address, ChannelWrite::Clear));
        }
        if writes.is_empty() {
            return Err(CommandError(
                "give at least one --store CHANNEL SOURCE or --clear CHANNEL".to_owned(),
            )
            .into());
        }
        let plan = MemoryChannelPlan::new(writes)?;
        let mut changes = Vec::new();
        for (address, write) in plan.writes() {
            let entry = access.entry(PhysicalChannel::from_address(address))?;
            let before = entry.channel().map_or_else(
                || "empty".to_owned(),
                |record| format!("{} {}", record.receive_frequency, record.mode),
            );
            let (after, record) = match write {
                ChannelWrite::Store(record) => (
                    format!(
                        "{} {}",
                        record.channel.receive_frequency, record.channel.mode
                    ),
                    Some(record.to_wire_fields()),
                ),
                ChannelWrite::Clear => ("empty".to_owned(), None),
            };
            changes.push(Change {
                address,
                before,
                after,
                record,
            });
        }
        let replacements = plan.page_replacements(image)?;
        Ok(Prepared {
            changes,
            replacements,
        })
    }
}

/// The record a store takes from `source`: `a` or `b` for that band's
/// current VFO record in PM Off, without lockout as a CAT store from `FO`
/// takes it, or a stored channel's record and lockout.
fn source_record(
    snapshot: &Snapshot,
    access: &ChannelAccess<'_>,
    source: &str,
) -> AppResult<CatMemoryChannelRecord> {
    let band = match source {
        "a" | "A" => Some(Band::A),
        "b" | "B" => Some(Band::B),
        _ => None,
    };
    if let Some(band) = band {
        let (memory, _) = vfo::captured(snapshot, SlotIndex::new(0)?)?;
        let record = memory.current(band).cat_record()?;
        return Ok(CatMemoryChannelRecord::from_channel(record, false));
    }
    let address = parse_address(source)?;
    let entry = access.entry(PhysicalChannel::from_address(address))?;
    let record = entry
        .channel()
        .ok_or_else(|| CommandError(format!("source channel {address} is empty")))?;
    Ok(CatMemoryChannelRecord::from_channel(
        record.cat_record()?,
        entry.flag().scan_lockout(),
    ))
}

pub(super) async fn run_apply(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ApplyRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::MemoryChannels)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edits.backup)?;
    let prepared = request.edits.prepare(&snapshot)?;
    print_changes(&prepared);
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already holds these channels; nothing was opened".to_owned(),
        )
        .into());
    }
    let plan = PagePlan::pages(
        PlanKind::MemoryChannels,
        snapshot.identity(),
        &prepared.replacements,
        description(&prepared),
    );
    page_apply::run_usb(
        endpoint,
        baud,
        &plan,
        request.output.as_deref(),
        &request.edits.backup,
    )
    .await
}

pub(super) fn preview(edits: &EditSelection) -> AppResult<()> {
    let prepared = edits.prepare(&Snapshot::load(&edits.backup)?)?;
    print_changes(&prepared);
    if prepared.replacements.is_empty() {
        output::line(format_args!(
            "The backup already holds these channels; apply would write nothing."
        ));
    } else {
        let pages: Vec<String> = prepared
            .replacements
            .iter()
            .map(|replacement| format!("{:#07X}", replacement.page().address().as_u32()))
            .collect();
        output::line(format_args!(
            "Preview only; mcp channels apply --apply writes pages {}.",
            pages.join(", ")
        ));
    }
    Ok(())
}

fn print_changes(prepared: &Prepared) {
    for change in &prepared.changes {
        output::line(format_args!(
            "Channel {}: {} -> {}.",
            change.address, change.before, change.after
        ));
    }
}

/// The journal description of an edit: each channel's contents before and
/// after, and the CAT fields of each stored record.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let changes: Vec<Value> = prepared
        .changes
        .iter()
        .map(|change| {
            json!({
                "channel": change.address.to_string(),
                "before": change.before,
                "after": change.after,
                "record": change.record,
            })
        })
        .collect();
    let mut description = Map::new();
    let _previous = description.insert("memory_channels".to_owned(), Value::Array(changes));
    description
}
