//! The D-STAR repeater and hotspot lists of a backup: offline listing and
//! preview, and one in-place record edit over one MCP session.
//!
//! `list` and `preview` read a completed backup report only; `set` writes the
//! edited record's page through the shared page workflow, which compares the
//! whole page before the write and reads it back.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kenwood_tmd750::memory::StoredText;
use kenwood_tmd750::memory::repeater_list::{
    GroupIndex, REPEATER_GROUP_NAMES_ADDRESS, RepeaterFrequency, RepeaterList, RepeaterListKind,
    RepeaterNames, RepeaterRecord, RepeaterShift, RepeaterSlot,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::StoredCallsign;
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// First byte of the repeater list's records.
const TABLES_START: u32 = 0x2_A000;

/// Repeater subcommand: offline listing and preview, or one in-place edit.
#[derive(Debug, Parser)]
pub(crate) struct RepeaterRequest {
    #[command(subcommand)]
    command: RepeaterCommand,
}

#[derive(Debug, Subcommand)]
enum RepeaterCommand {
    /// List a backup's repeater or hotspot list in the radio's order.
    List(ListRequest),
    /// Show one record before and after an edit; never writes.
    Preview(EditRequest),
    /// Edit one stored record in place, comparing and reading back its page.
    Set(SetRequest),
    /// List the country and group names with their voice codes; never
    /// writes.
    Names(voice::NamesRequest),
    /// Preview or write one country or group voice code.
    Voice(voice::VoiceRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// The hotspot list instead of the repeater list.
    #[arg(long)]
    hotspots: bool,
}

#[derive(Debug, Args)]
struct EditRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Edit the hotspot list instead of the repeater list.
    #[arg(long)]
    hotspots: bool,
    /// Slot number of a stored record, as `mcp repeater list` prints it.
    #[arg(long, value_name = "SLOT")]
    slot: u16,
    /// Output frequency in MHz.
    #[arg(long, value_name = "MHZ")]
    frequency: Option<String>,
    /// Input offset in MHz.
    #[arg(long, value_name = "MHZ")]
    offset: Option<String>,
    /// Direction of the input from the output.
    #[arg(long, value_enum)]
    shift: Option<Shift>,
    /// Name, at most 16 stored bytes.
    #[arg(long, allow_hyphen_values = true)]
    name: Option<String>,
    /// Sub-name, at most 16 stored bytes.
    #[arg(long, allow_hyphen_values = true)]
    sub_name: Option<String>,
    /// Repeater callsign with its module letter, at most eight characters.
    #[arg(long)]
    callsign: Option<String>,
    /// Gateway callsign, at most eight characters; empty for none.
    #[arg(long)]
    gateway: Option<String>,
}

/// Arguments of `mcp repeater set`.
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Shift {
    /// The input is below the output.
    Minus,
    /// The input is above the output.
    Plus,
}

/// One planned edit: the slot, the record before and after, and the page.
struct Prepared {
    slot: RepeaterSlot,
    before: RepeaterRecord,
    after: RepeaterRecord,
    replacement: PageReplacement,
}

/// The image span both lists and the name tables occupy.
fn tables() -> AppResult<Region> {
    let end = REPEATER_GROUP_NAMES_ADDRESS + u32::from(GroupIndex::COUNT) * 16;
    Ok(Region::new(TABLES_START, end)?)
}

const fn kind(hotspots: bool) -> RepeaterListKind {
    if hotspots {
        RepeaterListKind::Hotspot
    } else {
        RepeaterListKind::Repeater
    }
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when no field is given, the slot holds no record, or
    /// a value has no stored form.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        if self.frequency.is_none()
            && self.offset.is_none()
            && self.shift.is_none()
            && self.name.is_none()
            && self.sub_name.is_none()
            && self.callsign.is_none()
            && self.gateway.is_none()
        {
            return Err(CommandError(
                "give at least one of --frequency, --offset, --shift, --name, --sub-name, --callsign or --gateway".to_owned(),
            )
            .into());
        }
        let kind = kind(self.hotspots);
        let image = snapshot.image_covering(tables()?)?;
        let slot = RepeaterSlot::new(kind, self.slot)?;
        let list = RepeaterList::read(image, kind)?;
        let before = list.get(slot).cloned().ok_or_else(|| {
            CommandError(format!(
                "{slot} holds no record; list the stored slots with mcp repeater list"
            ))
        })?;
        let mut after = before.clone();
        if let Some(text) = &self.frequency {
            after.set_frequency(RepeaterFrequency::from_mhz_str(text)?);
        }
        if let Some(text) = &self.offset {
            after.set_offset(RepeaterFrequency::from_mhz_str(text)?);
        }
        if let Some(shift) = self.shift {
            after.set_shift(match shift {
                Shift::Minus => RepeaterShift::Minus,
                Shift::Plus => RepeaterShift::Plus,
            });
        }
        if let Some(text) = &self.name {
            after.set_name(StoredText::new(text)?);
        }
        if let Some(text) = &self.sub_name {
            after.set_sub_name(StoredText::new(text)?);
        }
        if let Some(text) = &self.callsign {
            after.set_callsign(StoredCallsign::new(text)?)?;
        }
        if let Some(text) = &self.gateway {
            after.set_gateway(StoredCallsign::new(text)?)?;
        }
        let replacement = after.page_replacement(image, slot)?;
        Ok(Prepared {
            slot,
            before,
            after,
            replacement,
        })
    }
}

impl RepeaterRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        let command = match &self.command {
            RepeaterCommand::Set(_) => "mcp repeater set",
            RepeaterCommand::Voice(voice) if voice.writes() => "mcp repeater voice set",
            _ => return Ok(()),
        };
        if explicit_port {
            Ok(())
        } else {
            Err(CommandError(format!("{command} requires an explicit --port before mcp")).into())
        }
    }
}

pub(super) fn run_offline(request: &RepeaterRequest) -> Option<AppResult<()>> {
    match &request.command {
        RepeaterCommand::List(list) => Some(print_list(list)),
        RepeaterCommand::Preview(edit) => Some(preview(edit)),
        RepeaterCommand::Names(names) => Some(voice::print_names(names)),
        RepeaterCommand::Voice(voice) => voice.run_offline(),
        RepeaterCommand::Set(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &RepeaterRequest,
) -> AppResult<()> {
    match &request.command {
        RepeaterCommand::Set(request) => run_set(endpoint, baud, request).await,
        RepeaterCommand::Voice(voice) => voice.run_selected(endpoint, baud).await,
        RepeaterCommand::List(list) => print_list(list),
        RepeaterCommand::Preview(edit) => preview(edit),
        RepeaterCommand::Names(names) => voice::print_names(names),
    }
}

async fn run_set(endpoint: &SerialCandidate, baud: u32, request: &SetRequest) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::Repeater)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    print_edit(&prepared);
    let plan = PagePlan::pages(
        PlanKind::Repeater,
        snapshot.identity(),
        std::slice::from_ref(&prepared.replacement),
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
    let snapshot = Snapshot::load(&edit.backup)?;
    let prepared = edit.prepare(&snapshot)?;
    print_edit(&prepared);
    output::line(format_args!(
        "Preview only; mcp repeater set --apply writes page {:#07X}.",
        prepared.replacement.page().address().as_u32()
    ));
    Ok(())
}

fn print_list(request: &ListRequest) -> AppResult<()> {
    let snapshot = Snapshot::load(&request.backup)?;
    let image = snapshot.image_covering(tables()?)?;
    let kind = kind(request.hotspots);
    let list = RepeaterList::read(image, kind)?;
    let names = RepeaterNames::read(image)?;
    output::line(format_args!(
        "{} {kind} records of {}.",
        list.len(),
        kind.capacity()
    ));
    for (slot, record) in list.iter() {
        let place = [
            record.region().map(|index| names.region(index).to_string()),
            record
                .country()
                .map(|index| names.country(index).to_string()),
            record.group().map(|index| names.group(index).to_string()),
        ]
        .map(Option::unwrap_or_default)
        .join(" / ");
        output::line(format_args!(
            "{:>4}  {}  [{place}]",
            slot.index(),
            summary(record)
        ));
    }
    Ok(())
}

/// One line describing a record's text and radio fields.
fn summary(record: &RepeaterRecord) -> String {
    let position = record.position();
    format!(
        "{} / {}  {} via {:?}  {} {}{}  {:.5},{:.5}  UTC{}",
        record.name(),
        record.sub_name(),
        record.callsign(),
        record.gateway().text(),
        record.frequency(),
        record.shift(),
        record.offset(),
        position.latitude.degrees(),
        position.longitude.degrees(),
        record.utc_offset()
    )
}

fn print_edit(prepared: &Prepared) {
    output::line(format_args!(
        "{} before: {}",
        prepared.slot,
        summary(&prepared.before)
    ));
    output::line(format_args!(
        "{} after:  {}",
        prepared.slot,
        summary(&prepared.after)
    ));
}

/// The journal description of an edit: the slot and both records' fields.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let fields = |record: &RepeaterRecord| {
        json!({
            "name": record.name().to_string(),
            "sub_name": record.sub_name().to_string(),
            "callsign": record.callsign().text(),
            "gateway": record.gateway().text(),
            "frequency_hz": record.frequency().hertz(),
            "offset_hz": record.offset().hertz(),
            "shift": u8::from(record.shift()),
        })
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "repeater".to_owned(),
        json!({
            "list": prepared.slot.kind().to_string(),
            "slot": prepared.slot.index(),
            "before": fields(&prepared.before),
            "after": fields(&prepared.after),
        }),
    );
    description
}

mod voice;

#[cfg(test)]
mod tests;
