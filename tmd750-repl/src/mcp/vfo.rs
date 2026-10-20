//! The VFO memory of a PM slot: an offline listing and preview, and one edit
//! over one MCP session.
//!
//! `list` prints a slot's current VFO record and selected VFO band for each
//! band, then every VFO band's record and programmable limits. An edit tunes
//! one band to a new receive frequency as the radio does, or sets one VFO
//! band's limits; `set` writes the changed pages through the shared page
//! workflow, which compares each whole page before its write and reads it
//! back.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::MenuFieldId;
use kenwood_tmd750::memory::vfo::{StepSegment, VfoBand, VfoMemory};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::{Band, Frequency, SlotIndex, StoredChannel};
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
pub(super) mod tests;

/// VFO subcommand: an offline listing or preview, or one guarded live write.
#[derive(Debug, Parser)]
pub(crate) struct VfoRequest {
    #[command(subcommand)]
    command: VfoCommand,
}

#[derive(Debug, Subcommand)]
enum VfoCommand {
    /// Print a slot's VFO memory; never writes.
    List(ListRequest),
    /// Show an edit's records and the pages it changes; never writes.
    Preview(EditRequest),
    /// Write an edit, comparing and reading back each changed page.
    Set(SetRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// PM slot, 0 to 5, as `mcp menu --slot` takes it.
    #[arg(long, default_value_t = 0)]
    slot: u8,
}

#[derive(Debug, Args)]
struct EditRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// PM slot, 0 to 5, as `mcp menu --slot` takes it.
    #[arg(long, default_value_t = 0)]
    slot: u8,
    /// Set `BAND`'s (`a` or `b`) frequency to `MHZ` as the radio's frequency
    /// entry does, taking the VFO band, step and shift that entry takes.
    #[arg(long, num_args = 2, value_names = ["BAND", "MHZ"], conflicts_with = "limits")]
    tune: Option<Vec<String>>,
    /// Set the programmable limits of `VFO_BAND` (`a118`, `a144`, `a220`,
    /// `a430`, `b118`, `b144`, `b-vhf`, `b200-300`, `b430` or `b-uhf`) to
    /// `LOWER` through `UPPER` whole megahertz.
    #[arg(long, num_args = 3, value_names = ["VFO_BAND", "LOWER", "UPPER"])]
    limits: Option<Vec<String>>,
}

/// Arguments of `mcp vfo set`.
#[derive(Debug, Args)]
pub(super) struct SetRequest {
    #[command(flatten)]
    edit: EditRequest,
    /// Required. The edit stays on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// The command-line and journal spelling of a VFO band.
const fn label(band: VfoBand) -> &'static str {
    match band {
        VfoBand::A118 => "a118",
        VfoBand::A144 => "a144",
        VfoBand::A220 => "a220",
        VfoBand::A430 => "a430",
        VfoBand::B118 => "b118",
        VfoBand::B144 => "b144",
        VfoBand::BVhf => "b-vhf",
        VfoBand::B200And300 => "b200-300",
        VfoBand::B430 => "b430",
        VfoBand::BUhf => "b-uhf",
    }
}

const fn band_label(band: Band) -> &'static str {
    match band {
        Band::A => "a",
        Band::B => "b",
    }
}

fn parse_band(text: &str) -> AppResult<Band> {
    match text {
        "a" | "A" => Ok(Band::A),
        "b" | "B" => Ok(Band::B),
        other => Err(CommandError(format!("band must be a or b, not {other:?}")).into()),
    }
}

fn parse_vfo_band(text: &str) -> AppResult<VfoBand> {
    VfoBand::ALL
        .into_iter()
        .find(|band| label(*band) == text)
        .ok_or_else(|| {
            CommandError(format!(
                "VFO band must be one of {}, not {text:?}",
                VfoBand::ALL.map(label).join(", ")
            ))
            .into()
        })
}

fn parse_mhz(text: &str) -> AppResult<u16> {
    text.parse()
        .map_err(|_| CommandError(format!("{text:?} is not a whole number of megahertz")).into())
}

/// What an edit changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// A band tuned into a VFO band.
    Tune(Band, VfoBand),
    /// A VFO band's limits.
    Limits(VfoBand),
}

/// One planned edit: the slot's memory before and after, and the page writes.
struct Prepared {
    before: VfoMemory,
    after: VfoMemory,
    change: Change,
    replacements: Vec<PageReplacement>,
}

/// The slot's memory decoded from a backup that captured its VFO pages and
/// its Auto Repeater Offset setting.
pub(super) fn captured(
    snapshot: &Snapshot,
    slot: SlotIndex,
) -> AppResult<(VfoMemory, &kenwood_tmd750::MemoryImage)> {
    let start = 0x5_0100 + slot.offset();
    let image = snapshot.image_covering(Region::new(start, start + 0x400)?)?;
    let auto_offset = MenuFieldId::RADIO_AUTO_OFFSET
        .field()
        .descriptor
        .address(Some(slot))?
        .as_u32();
    let _covered = snapshot.image_covering(Region::new(auto_offset, auto_offset + 1)?)?;
    Ok((VfoMemory::read(image, slot)?, image))
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when neither or both of `--tune` and `--limits` are
    /// given, a value does not parse, or the library refuses the edit.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let slot = SlotIndex::new(self.slot)?;
        let (before, image) = captured(snapshot, slot)?;
        let mut after = before.clone();
        let change = match (self.tune.as_deref(), self.limits.as_deref()) {
            (Some([band, mhz]), None) => {
                let band = parse_band(band)?;
                let frequency = Frequency::from_mhz_str(mhz)?;
                Change::Tune(band, after.tune_frequency(band, frequency)?)
            }
            (None, Some([name, lower, upper])) => {
                let band = parse_vfo_band(name)?;
                after.set_limits(band, parse_mhz(lower)?, parse_mhz(upper)?)?;
                Change::Limits(band)
            }
            _ => {
                return Err(CommandError(
                    "give --tune BAND MHZ or --limits VFO_BAND LOWER UPPER".to_owned(),
                )
                .into());
            }
        };
        let replacements = after.page_replacements(image)?;
        Ok(Prepared {
            before,
            after,
            change,
            replacements,
        })
    }
}

impl VfoRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, VfoCommand::Set(_)) && !explicit_port {
            return Err(CommandError(
                "mcp vfo set requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &VfoRequest) -> Option<AppResult<()>> {
    match &request.command {
        VfoCommand::List(list) => Some(print_list(list)),
        VfoCommand::Preview(edit) => Some(preview(edit)),
        VfoCommand::Set(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &VfoRequest,
) -> AppResult<()> {
    match &request.command {
        VfoCommand::Set(request) => run_set(endpoint, baud, request).await,
        VfoCommand::List(list) => print_list(list),
        VfoCommand::Preview(edit) => preview(edit),
    }
}

async fn run_set(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &SetRequest,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::Vfo)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    print_edit(&prepared);
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already holds this VFO memory; nothing was opened".to_owned(),
        )
        .into());
    }
    let plan = PagePlan::pages(
        PlanKind::Vfo,
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
    let slot = SlotIndex::new(request.slot)?;
    let (memory, _image) = captured(&Snapshot::load(&request.backup)?, slot)?;
    for line in list_lines(&memory) {
        output::line(format_args!("{line}"));
    }
    Ok(())
}

/// The listing of a slot's VFO memory: each band's current record and VFO
/// band, every VFO band's record and limits, the Auto Repeater Offset setting
/// and the receive step of each band B step range, a repeated range once.
fn list_lines(memory: &VfoMemory) -> Vec<String> {
    let mut lines = Vec::new();
    for band in [Band::A, Band::B] {
        let current = memory.current(band);
        lines.push(format!(
            "Slot {} band {band}: {} {}, VFO band {}.",
            memory.slot().index(),
            current.receive_frequency,
            current.mode,
            memory.selected(band)
        ));
    }
    for stored in memory.bands() {
        let limits = stored.limits();
        lines.push(format!(
            "  {}: {} {}, limits {} to {} MHz.",
            stored.band(),
            stored.record().receive_frequency,
            stored.record().mode,
            limits.lower_mhz(),
            limits.upper_mhz()
        ));
    }
    lines.push(format!(
        "Auto Repeater Offset: {}.",
        if memory.auto_repeater_offset() {
            "on"
        } else {
            "off"
        }
    ));
    let mut listed = Vec::new();
    let mut steps = Vec::new();
    for segment in StepSegment::ALL {
        let (lower, end) = segment.range_mhz();
        if listed.contains(&(lower, end)) {
            continue;
        }
        listed.push((lower, end));
        let step = memory
            .segment_step(segment)
            .map_or_else(|| "no step".to_owned(), |step| step.to_string());
        steps.push(format!("{lower}-{end} MHz {step}"));
    }
    lines.push(format!("Band B steps: {}.", steps.join(", ")));
    lines
}

fn preview(edit: &EditRequest) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_edit(&prepared);
    output::line(format_args!(
        "Preview only; mcp vfo set --apply writes {} pages.",
        prepared.replacements.len()
    ));
    Ok(())
}

fn print_edit(prepared: &Prepared) {
    let show = |record: &StoredChannel| {
        record.cat_record().map_or_else(
            |_| record.receive_frequency.to_string(),
            |cat| cat.to_string(),
        )
    };
    match prepared.change {
        Change::Tune(band, vfo_band) => output::line(format_args!(
            "Band {band}: {} -> {} in VFO band {vfo_band}.",
            show(prepared.before.current(band)),
            show(prepared.after.current(band))
        )),
        Change::Limits(band) => {
            let [before, after] = [&prepared.before, &prepared.after].map(|memory| {
                let limits = memory.band(band).limits();
                format!("{} to {} MHz", limits.lower_mhz(), limits.upper_mhz())
            });
            output::line(format_args!("{band} limits: {before} -> {after}."));
        }
    }
    for replacement in &prepared.replacements {
        output::line(format_args!(
            "Page {:#07X} changes.",
            replacement.page().address().as_u32()
        ));
    }
}

/// The journal description of an edit: the slot and the change's values
/// before and after.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let change = match prepared.change {
        Change::Tune(band, vfo_band) => json!({
            "tune": {
                "band": band_label(band),
                "vfo_band": label(vfo_band),
                "before_hz": prepared.before.current(band).receive_frequency.as_hz(),
                "after_hz": prepared.after.current(band).receive_frequency.as_hz(),
            }
        }),
        Change::Limits(band) => {
            let pair = |memory: &VfoMemory| {
                let limits = memory.band(band).limits();
                json!([limits.lower_mhz(), limits.upper_mhz()])
            };
            json!({
                "limits": {
                    "vfo_band": label(band),
                    "before_mhz": pair(&prepared.before),
                    "after_mhz": pair(&prepared.after),
                }
            })
        }
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "vfo".to_owned(),
        json!({"slot": prepared.after.slot().index(), "change": change}),
    );
    description
}
