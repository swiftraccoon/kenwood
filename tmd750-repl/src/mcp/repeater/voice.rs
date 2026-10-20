//! The repeater list's country and group names with their voice codes: an
//! offline listing, and one voice code edit over one MCP session.
//!
//! `set` writes the code's page through the shared page workflow, which
//! compares the whole page before the write and reads it back.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::repeater_list::{
    CountryIndex, GroupIndex, REPEATER_COUNTRY_VOICE_ADDRESS, REPEATER_GROUP_VOICE_ADDRESS,
    RepeaterNames, VOICE_CODE_SIZE, VoiceCode,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::tables;
use crate::mcp::page_apply::{self, PagePlan, PlanKind};
use crate::mcp::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

#[cfg(test)]
mod tests;

/// Arguments of `mcp repeater names`.
#[derive(Debug, Args)]
pub(super) struct NamesRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
}

/// Voice code subcommand: an offline preview or one guarded live write.
#[derive(Debug, Parser)]
pub(super) struct VoiceRequest {
    #[command(subcommand)]
    command: VoiceCommand,
}

#[derive(Debug, Subcommand)]
enum VoiceCommand {
    /// Show one voice code before and after an edit; never writes.
    Preview(VoiceEdit),
    /// Write one voice code, comparing and reading back its page.
    Set(VoiceSet),
}

#[derive(Debug, Args)]
struct VoiceEdit {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Country index, 0 to 149, as `mcp repeater names` prints it.
    #[arg(long, value_name = "INDEX", conflicts_with = "group")]
    country: Option<u8>,
    /// Group index, 0 to 299, as `mcp repeater names` prints it.
    #[arg(long, value_name = "INDEX")]
    group: Option<u16>,
    /// Up to four uppercase letters and digits.
    #[arg(long, conflicts_with = "clear")]
    code: Option<String>,
    /// Clear the code instead.
    #[arg(long)]
    clear: bool,
}

/// Arguments of `mcp repeater voice set`.
#[derive(Debug, Args)]
struct VoiceSet {
    #[command(flatten)]
    edit: VoiceEdit,
    /// Required. The edited code stays on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// The table entry an edit names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Country(CountryIndex),
    Group(GroupIndex),
}

/// One planned edit: the entry and its name, the code before and after, and
/// the page writes.
struct Prepared {
    entry: Entry,
    name: String,
    before: VoiceCode,
    after: VoiceCode,
    replacements: Vec<PageReplacement>,
}

/// The names and voice codes of a backup that captured both.
fn captured(snapshot: &Snapshot) -> AppResult<(RepeaterNames, &kenwood_tmd750::MemoryImage)> {
    let end = REPEATER_GROUP_VOICE_ADDRESS
        + u32::from(GroupIndex::COUNT) * u32::try_from(VOICE_CODE_SIZE)?;
    let _voices = snapshot.image_covering(Region::new(REPEATER_COUNTRY_VOICE_ADDRESS, end)?)?;
    let image = snapshot.image_covering(tables()?)?;
    Ok((RepeaterNames::read(image)?, image))
}

impl VoiceEdit {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error unless exactly one of `--country` and `--group` and
    /// one of `--code` and `--clear` is given, for a valid index and code.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let entry = match (self.country, self.group) {
            (Some(index), None) => Entry::Country(CountryIndex::new(index)?),
            (None, Some(index)) => Entry::Group(GroupIndex::new(index)?),
            _ => {
                return Err(
                    CommandError("give --country INDEX or --group INDEX".to_owned()).into(),
                );
            }
        };
        let after = match (&self.code, self.clear) {
            (Some(code), false) => VoiceCode::new(code)?,
            (None, true) => VoiceCode::EMPTY,
            _ => return Err(CommandError("give --code CODE or --clear".to_owned()).into()),
        };
        let (mut names, image) = captured(snapshot)?;
        let (name, before) = match entry {
            Entry::Country(index) => {
                let before = *names.country_voice(index);
                names.set_country_voice(index, after);
                (names.country(index).to_string(), before)
            }
            Entry::Group(index) => {
                let before = *names.group_voice(index);
                names.set_group_voice(index, after);
                (names.group(index).to_string(), before)
            }
        };
        Ok(Prepared {
            entry,
            name,
            before,
            after,
            replacements: names.page_replacements(image)?,
        })
    }
}

impl VoiceRequest {
    pub(super) const fn writes(&self) -> bool {
        matches!(self.command, VoiceCommand::Set(_))
    }

    pub(super) fn run_offline(&self) -> Option<AppResult<()>> {
        match &self.command {
            VoiceCommand::Preview(edit) => Some(preview(edit)),
            VoiceCommand::Set(_) => None,
        }
    }

    pub(super) async fn run_selected(
        &self,
        endpoint: &SerialCandidate,
        baud: std::num::NonZeroU32,
    ) -> AppResult<()> {
        match &self.command {
            VoiceCommand::Set(request) => run_set(endpoint, baud, request).await,
            VoiceCommand::Preview(edit) => preview(edit),
        }
    }
}

async fn run_set(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &VoiceSet,
) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::Repeater)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    print_edit(&prepared);
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already holds this voice code; nothing was opened".to_owned(),
        )
        .into());
    }
    let plan = PagePlan::pages(
        PlanKind::Repeater,
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

fn preview(edit: &VoiceEdit) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_edit(&prepared);
    if prepared.replacements.is_empty() {
        output::line(format_args!(
            "The backup already holds this voice code; set would write nothing."
        ));
    } else {
        output::line(format_args!(
            "Preview only; mcp repeater voice set --apply writes page {:#07X}.",
            prepared
                .replacements
                .first()
                .map_or(0, |replacement| replacement.page().address().as_u32())
        ));
    }
    Ok(())
}

pub(super) fn print_names(request: &NamesRequest) -> AppResult<()> {
    let (names, _image) = captured(&Snapshot::load(&request.backup)?)?;
    for line in name_lines(&names) {
        output::line(format_args!("{line}"));
    }
    Ok(())
}

/// Every named or coded country and group with its index, name and voice
/// code.
fn name_lines(names: &RepeaterNames) -> Vec<String> {
    let show = |code: &VoiceCode| {
        if code.is_empty() {
            "no voice code".to_owned()
        } else {
            format!("voice code {code}")
        }
    };
    let mut lines = vec!["Countries:".to_owned()];
    for index in (0..CountryIndex::COUNT).filter_map(|index| CountryIndex::new(index).ok()) {
        let (name, code) = (names.country(index), names.country_voice(index));
        if !name.is_empty() || !code.is_empty() {
            lines.push(format!("  {:>3} {name}: {}", index.get(), show(code)));
        }
    }
    lines.push("Groups:".to_owned());
    for index in (0..GroupIndex::COUNT).filter_map(|index| GroupIndex::new(index).ok()) {
        let (name, code) = (names.group(index), names.group_voice(index));
        if !name.is_empty() || !code.is_empty() {
            lines.push(format!("  {:>3} {name}: {}", index.get(), show(code)));
        }
    }
    lines
}

fn print_edit(prepared: &Prepared) {
    let (table, index) = match prepared.entry {
        Entry::Country(index) => ("Country", u16::from(index.get())),
        Entry::Group(index) => ("Group", index.get()),
    };
    output::line(format_args!(
        "{table} {index} {}: voice code {:?} -> {:?}.",
        prepared.name,
        prepared.before.as_str(),
        prepared.after.as_str()
    ));
}

/// The journal description of an edit: the entry and its code before and
/// after.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let (table, index) = match prepared.entry {
        Entry::Country(index) => ("country", u16::from(index.get())),
        Entry::Group(index) => ("group", index.get()),
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "voice".to_owned(),
        json!({
            "table": table,
            "index": index,
            "name": prepared.name,
            "before": prepared.before.as_str(),
            "after": prepared.after.as_str(),
        }),
    );
    description
}
