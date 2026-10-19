//! The user Mic-E type code tables: an offline listing and preview, and one
//! entry write over one MCP session.
//!
//! An edit stores or clears one entry of one table, starting from the
//! backup's tables; `apply` writes the tables' page through the shared page
//! workflow, which compares the whole page before the write and reads it
//! back.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kenwood_tmd750::memory::mic_e::{
    MIC_E_TABLE_ENTRIES, MicEFamily, MicETypeCode, MicETypeCodeTables,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// The image span of the page holding the three tables.
const TABLES: (u32, u32) = (0x800, 0x900);

/// Mic-E subcommand: an offline listing or preview, or one guarded live
/// write.
#[derive(Debug, Parser)]
pub(crate) struct MicERequest {
    #[command(subcommand)]
    command: MicECommand,
}

#[derive(Debug, Subcommand)]
enum MicECommand {
    /// Print the backup's three tables; never writes.
    List(ListRequest),
    /// Show one entry before and after an edit; never writes.
    Preview(EditRequest),
    /// Write one edited entry, comparing and reading back the tables' page.
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
    /// The table to edit.
    #[arg(long, value_enum)]
    family: Family,
    /// Entry position, 1 to 5, as `list` numbers it.
    #[arg(long, value_name = "POSITION")]
    position: usize,
    /// Type code: one printable ASCII character other than space.
    #[arg(long, requires = "model", conflicts_with = "clear")]
    code: Option<char>,
    /// Model name: at most 15 printable ASCII characters.
    #[arg(long, requires = "code", allow_hyphen_values = true)]
    model: Option<String>,
    /// Clear the entry instead.
    #[arg(long)]
    clear: bool,
}

/// Arguments of `mcp mic-e apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    edit: EditRequest,
    /// Required. The edited entry stays on the radio; nothing is rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// A table as the command line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Family {
    /// Kenwood handheld codes.
    KenwoodHandheld,
    /// Kenwood mobile codes.
    KenwoodMobile,
    /// Yaesu codes.
    Yaesu,
}

impl Family {
    const fn library(self) -> MicEFamily {
        match self {
            Self::KenwoodHandheld => MicEFamily::KenwoodHandheld,
            Self::KenwoodMobile => MicEFamily::KenwoodMobile,
            Self::Yaesu => MicEFamily::Yaesu,
        }
    }
}

/// The journal spelling of a table, as the command line names it.
const fn label(family: MicEFamily) -> &'static str {
    match family {
        MicEFamily::KenwoodHandheld => "kenwood-handheld",
        MicEFamily::KenwoodMobile => "kenwood-mobile",
        MicEFamily::Yaesu => "yaesu",
    }
}

/// One planned edit: the entry before and after, and the page write.
struct Prepared {
    family: MicEFamily,
    position: usize,
    before: Option<MicETypeCode>,
    after: Option<MicETypeCode>,
    replacement: Option<PageReplacement>,
}

/// The tables decoded from a backup that captured their page.
fn captured(snapshot: &Snapshot) -> AppResult<(MicETypeCodeTables, &kenwood_tmd750::MemoryImage)> {
    let image = snapshot.image_covering(Region::new(TABLES.0, TABLES.1)?)?;
    Ok((MicETypeCodeTables::read(image)?, image))
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when neither `--code`/`--model` nor `--clear` is
    /// given, the position is outside 1 to 5, or the entry is invalid.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let after = match (&self.code, &self.model, self.clear) {
            (Some(code), Some(model), false) => Some(MicETypeCode::new(*code, model)?),
            (None, None, true) => None,
            _ => {
                return Err(
                    CommandError("give --code CODE --model NAME, or --clear".to_owned()).into(),
                );
            }
        };
        let position = self
            .position
            .checked_sub(1)
            .filter(|&index| index < MIC_E_TABLE_ENTRIES)
            .ok_or_else(|| {
                CommandError(format!(
                    "position {} is not 1 to {MIC_E_TABLE_ENTRIES}",
                    self.position
                ))
            })?;
        let family = self.family.library();
        let (mut tables, image) = captured(snapshot)?;
        let before = tables.get(family, position).copied();
        tables.set(family, position, after)?;
        let replacement = tables.page_replacements(image)?.into_iter().next();
        Ok(Prepared {
            family,
            position,
            before,
            after,
            replacement,
        })
    }
}

impl MicERequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, MicECommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp mic-e apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &MicERequest) -> Option<AppResult<()>> {
    match &request.command {
        MicECommand::List(list) => Some(print_list(list)),
        MicECommand::Preview(edit) => Some(preview(edit)),
        MicECommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &MicERequest,
) -> AppResult<()> {
    match &request.command {
        MicECommand::Apply(request) => run_apply(endpoint, baud, request).await,
        MicECommand::List(list) => print_list(list),
        MicECommand::Preview(edit) => preview(edit),
    }
}

async fn run_apply(endpoint: &SerialCandidate, baud: u32, request: &ApplyRequest) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::MicE)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    print_edit(&prepared);
    let Some(replacement) = &prepared.replacement else {
        return Err(CommandError(
            "the backup already holds this entry; nothing was opened".to_owned(),
        )
        .into());
    };
    let plan = PagePlan::pages(
        PlanKind::MicE,
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

fn print_list(request: &ListRequest) -> AppResult<()> {
    let (tables, _image) = captured(&Snapshot::load(&request.backup)?)?;
    for family in MicEFamily::ALL {
        output::line(format_args!("{family}:"));
        for (position, entry) in (1..).zip(tables.table(family)) {
            match entry {
                Some(entry) => output::line(format_args!("  {position}. {entry}")),
                None => output::line(format_args!("  {position}. unused")),
            }
        }
    }
    Ok(())
}

fn preview(edit: &EditRequest) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_edit(&prepared);
    if prepared.replacement.is_some() {
        output::line(format_args!(
            "Preview only; mcp mic-e apply --apply writes page 0x00800."
        ));
    } else {
        output::line(format_args!(
            "The backup already holds this entry; apply would write nothing."
        ));
    }
    Ok(())
}

fn print_edit(prepared: &Prepared) {
    let show = |entry: Option<MicETypeCode>| {
        entry.map_or_else(|| "unused".to_owned(), |entry| entry.to_string())
    };
    let position = prepared.position + 1;
    output::line(format_args!(
        "{} {position} before: {}",
        prepared.family,
        show(prepared.before)
    ));
    output::line(format_args!(
        "{} {position} after:  {}",
        prepared.family,
        show(prepared.after)
    ));
}

/// The journal description of an edit: the table, the position and both
/// entries.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let entry = |entry: Option<MicETypeCode>| {
        entry.map_or(
            Value::Null,
            |entry| json!({"code": entry.code().to_string(), "model": entry.model()}),
        )
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "mic_e".to_owned(),
        json!({
            "family": label(prepared.family),
            "position": prepared.position + 1,
            "before": entry(prepared.before),
            "after": entry(prepared.after),
        }),
    );
    description
}

#[cfg(test)]
mod tests;
