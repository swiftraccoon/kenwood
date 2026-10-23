//! The GPS position memory: an offline listing and preview, and one write of
//! the edited memory over one MCP session.
//!
//! A backup holds the position memory when `mcp backup --position-memory`
//! took it. An edit starts from the backup's positions in index order:
//! `--remove` drops the positions at the given numbers and `--add` appends
//! positions stamped with the current UTC time. The result is stored compact
//! from slot 0 through the shared page workflow, which compares each whole
//! page before its write, so a memory changed on the radio since the backup
//! stops the run before any write.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::StoredText;
use kenwood_tmd750::memory::position::{
    POSITION_MEMORY_ENTRIES, PositionMemory, PositionSymbol, PositionTime, StoredPosition,
};
use kenwood_tmd750::protocol::mcp::regions;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::{
    Coordinate, Latitude, LatitudeHemisphere, Longitude, LongitudeHemisphere,
};
use kenwood_tmd750::{MemoryImage, PageReplacement};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
mod tests;

/// Position memory subcommand: an offline listing or preview, or one guarded
/// live write.
#[derive(Debug, Parser)]
pub(crate) struct PositionRequest {
    #[command(subcommand)]
    command: PositionCommand,
}

#[derive(Debug, Subcommand)]
enum PositionCommand {
    /// Print a backup's stored positions; never writes.
    List(ListRequest),
    /// Show the positions an edit leaves; never writes.
    Preview(EditRequest),
    /// Write the edited memory in one session, comparing and reading back
    /// each page.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful report.json of `mcp backup --position-memory`.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
}

#[derive(Debug, Args)]
struct EditRequest {
    /// Successful report.json of `mcp backup --position-memory`.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Remove the position numbered NUMBER, counted from 1 as `list` numbers
    /// them; repeatable.
    #[arg(long = "remove", value_name = "NUMBER", action = clap::ArgAction::Append)]
    remove: Vec<usize>,
    /// Append a position: NAME of at most nine stored bytes, LATITUDE and
    /// LONGITUDE in signed decimal degrees (negative south and west);
    /// repeatable.
    #[arg(
        long = "add",
        num_args = 3,
        value_names = ["NAME", "LATITUDE", "LONGITUDE"],
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    add: Vec<String>,
    /// Altitude stored with each added position, as the radio stores it.
    #[arg(
        long,
        value_name = "VALUE",
        default_value_t = 0,
        allow_negative_numbers = true
    )]
    altitude: i32,
    /// APRS symbol of each added position: the table, then the code.
    #[arg(long, value_name = "TABLE_CODE", default_value = "\\K")]
    symbol: String,
}

/// Arguments of `mcp position apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    edit: EditRequest,
    /// Required. The positions stay on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// A planned edit: the memory before and after, and the pages that change.
struct Prepared {
    before: PositionMemory,
    after: PositionMemory,
    replacements: Vec<PageReplacement>,
}

/// The memory decoded from a backup that captured both of its regions.
fn captured(snapshot: &Snapshot) -> AppResult<(PositionMemory, &MemoryImage)> {
    if !regions::POSITION_MEMORY
        .iter()
        .all(|region| snapshot.configuration().covers(*region))
    {
        return Err(CommandError(
            "the backup holds no position memory; take one with mcp backup --position-memory"
                .to_owned(),
        )
        .into());
    }
    let [records, _index] = regions::POSITION_MEMORY;
    let image = snapshot.image_covering(records)?;
    Ok((PositionMemory::read(image)?, image))
}

/// Geographic role and its original decimal-degree limit.
#[derive(Clone, Copy, Debug)]
enum CoordinateKind {
    Latitude,
    Longitude,
}

impl CoordinateKind {
    const fn limit(self) -> u64 {
        match self {
            Self::Latitude => 90,
            Self::Longitude => 180,
        }
    }
}

/// Signed decimal degrees, with at most three whole and nine fractional
/// digits, as a negative flag and the magnitude rounded to the nearest
/// ten-thousandth of a minute.
fn parse_degrees(text: &str, kind: CoordinateKind) -> AppResult<(bool, Coordinate)> {
    let invalid = || {
        CommandError(format!(
            "{text:?} is not decimal degrees with at most three whole and nine fractional digits"
        ))
    };
    let (negative, digits) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || whole.len() > 3
        || fraction.len() > 9
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid().into());
    }
    let whole: u64 = whole.parse().map_err(|_| invalid())?;
    let mut nanodegrees = whole * 1_000_000_000;
    for (digit, scale) in fraction.bytes().zip([
        100_000_000,
        10_000_000,
        1_000_000,
        100_000,
        10_000,
        1_000,
        100,
        10,
        1,
    ]) {
        nanodegrees += u64::from(digit - b'0') * scale;
    }
    if nanodegrees > kind.limit() * 1_000_000_000 {
        let name = match kind {
            CoordinateKind::Latitude => "latitude",
            CoordinateKind::Longitude => "longitude",
        };
        return Err(
            CommandError(format!("{name} degrees {text:?} exceed {}", kind.limit())).into(),
        );
    }
    let units = (nanodegrees * 6 + 5_000) / 10_000;
    let coordinate = Coordinate::new(
        u8::try_from(units / 600_000).map_err(|_| invalid())?,
        u8::try_from(units % 600_000 / 10_000)?,
        u16::try_from(units % 10_000)?,
    )?;
    Ok((negative, coordinate))
}

/// The current UTC time, to the second.
fn now() -> AppResult<PositionTime> {
    let time = OffsetDateTime::now_utc();
    Ok(PositionTime::new(
        u16::try_from(time.year())?,
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute(),
        time.second(),
    )?)
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when no edit is given, a number is outside the
    /// memory or repeated, a name, coordinate or symbol is invalid, or the
    /// memory would exceed its positions.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        if self.remove.is_empty() && self.add.is_empty() {
            return Err(CommandError(
                "give at least one --add NAME LATITUDE LONGITUDE or --remove NUMBER".to_owned(),
            )
            .into());
        }
        let (before, image) = captured(snapshot)?;
        let mut kept: Vec<Option<StoredPosition>> =
            before.entries().iter().cloned().map(Some).collect();
        for &number in &self.remove {
            let entry = number
                .checked_sub(1)
                .and_then(|index| kept.get_mut(index))
                .ok_or_else(|| {
                    CommandError(format!(
                        "position {number} is not in the backup's {} positions",
                        before.entries().len()
                    ))
                })?;
            if entry.take().is_none() {
                return Err(
                    CommandError(format!("position {number} is removed more than once")).into(),
                );
            }
        }
        let mut after = PositionMemory::from_entries(kept.into_iter().flatten().collect())?;
        if !self.add.is_empty() {
            let symbol = match self.symbol.as_bytes() {
                [table, code] => PositionSymbol::new(*table, *code)?,
                _ => {
                    return Err(CommandError(format!(
                        "symbol {:?} is not a table byte then a code byte",
                        self.symbol
                    ))
                    .into());
                }
            };
            let time = now()?;
            for [name, latitude, longitude] in self.add.as_chunks::<3>().0 {
                let (south, latitude) = parse_degrees(latitude, CoordinateKind::Latitude)?;
                let (west, longitude) = parse_degrees(longitude, CoordinateKind::Longitude)?;
                after.push(StoredPosition::new(
                    StoredText::new(name)?,
                    time,
                    Latitude::new(
                        if south {
                            LatitudeHemisphere::South
                        } else {
                            LatitudeHemisphere::North
                        },
                        latitude,
                    )?,
                    Longitude::new(
                        if west {
                            LongitudeHemisphere::West
                        } else {
                            LongitudeHemisphere::East
                        },
                        longitude,
                    )?,
                    self.altitude,
                    symbol,
                ))?;
            }
        }
        let replacements = after.page_replacements(image)?;
        Ok(Prepared {
            before,
            after,
            replacements,
        })
    }
}

impl PositionRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, PositionCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp position apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &PositionRequest) -> Option<AppResult<()>> {
    match &request.command {
        PositionCommand::List(list) => Some(print_list(list)),
        PositionCommand::Preview(edit) => Some(preview(edit)),
        PositionCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &PositionRequest,
) -> AppResult<()> {
    match &request.command {
        PositionCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        PositionCommand::List(list) => print_list(list),
        PositionCommand::Preview(edit) => preview(edit),
    }
}

async fn run_apply(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &ApplyRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::PositionMemory)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already contains the edited positions; nothing was opened".to_owned(),
        )
        .into());
    }
    print_entries("After", &prepared.after);
    print_pages(&prepared);
    let plan = PagePlan::pages(
        PlanKind::PositionMemory,
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
    let (memory, _image) = captured(&Snapshot::load(&request.backup)?)?;
    print_entries("Backup", &memory);
    Ok(())
}

fn preview(edit: &EditRequest) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_entries("After", &prepared.after);
    print_pages(&prepared);
    output::line(format_args!(
        "Preview only; mcp position apply --apply writes {} pages.",
        prepared.replacements.len()
    ));
    Ok(())
}

fn print_entries(heading: &str, memory: &PositionMemory) {
    output::line(format_args!(
        "{heading}: {} of {POSITION_MEMORY_ENTRIES} positions.",
        memory.entries().len()
    ));
    for (number, entry) in (1..).zip(memory.entries()) {
        output::line(format_args!(
            "{number:>3}. {:?} {:.6} {:.6} altitude {} symbol {} at {}.",
            entry.name.to_string(),
            entry.latitude.degrees(),
            entry.longitude.degrees(),
            entry.altitude,
            entry.symbol,
            entry.time
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

/// The journal description of a plan: the positions before and after.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let entries = |memory: &PositionMemory| -> Value {
        memory
            .entries()
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.name.to_string(),
                    "latitude": entry.latitude.degrees(),
                    "longitude": entry.longitude.degrees(),
                    "altitude": entry.altitude,
                    "symbol": entry.symbol.to_string(),
                    "time": entry.time.to_string(),
                })
            })
            .collect()
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "position_memory".to_owned(),
        json!({
            "before": entries(&prepared.before),
            "after": entries(&prepared.after),
        }),
    );
    description
}
