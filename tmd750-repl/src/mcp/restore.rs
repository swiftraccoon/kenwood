//! Configuration restore: an offline preview, and one write of every page a
//! restore changes over one MCP session.
//!
//! The pages are planned from the radio's current standard backup and the
//! configuration to restore, another standard backup with the same identity
//! or a `.d750` file for the same radio type, and written through the shared
//! page workflow, which compares each whole page before its write and reads
//! it back. Lifecycle settings keep their current values; the preview and the
//! journal list each one the restored configuration holds differently.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::file::FILE_SIZE_FULL;
use kenwood_tmd750::memory::restore::RestorePlan;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{RadioConfig, parse_d750};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// Pages a preview lists by address before summarizing the rest.
const LISTED_PAGES: usize = 16;

/// Restore subcommand: an offline preview, or one guarded live write.
#[derive(Debug, Parser)]
pub(crate) struct RestoreRequest {
    #[command(subcommand)]
    command: RestoreCommand,
}

#[derive(Debug, Subcommand)]
enum RestoreCommand {
    /// Show the pages a restore writes and the settings it keeps; never
    /// writes.
    Preview(RestoreSelection),
    /// Write the restored pages in one session, comparing and reading back
    /// each one.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct RestoreSelection {
    /// Successful standard configuration-backup report.json of the radio as
    /// it is now.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Successful standard configuration-backup report.json to restore, with
    /// the same model, firmware and type.
    #[arg(long, value_name = "REPORT", conflicts_with = "from_file")]
    from: Option<PathBuf>,
    /// `.d750` configuration file to restore, for the same radio type; only
    /// its bytes in the standard configuration schedule are read.
    #[arg(long, value_name = "FILE")]
    from_file: Option<PathBuf>,
}

/// The configuration a restore takes its bytes from.
enum Source<'a> {
    /// A standard backup report.
    Backup(&'a Path),
    /// A `.d750` file.
    File(&'a Path),
}

/// Read a `.d750` file of at most a full file's length.
fn read_d750(path: &Path) -> AppResult<RadioConfig> {
    let unreadable = |error: std::io::Error| {
        CommandError(format!("{} could not be read: {error}", path.display()))
    };
    let limit = u64::try_from(FILE_SIZE_FULL)?;
    let mut bytes = Vec::new();
    let _read = File::open(path)
        .map_err(unreadable)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    Ok(parse_d750(&bytes)?)
}

/// Arguments of `mcp restore apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    selection: RestoreSelection,
    /// Required. The restored configuration stays on the radio; nothing is
    /// rolled back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

impl RestoreSelection {
    /// The configuration to restore.
    ///
    /// Returns an error unless exactly one of `--from` and `--from-file` is
    /// given.
    fn source(&self) -> AppResult<Source<'_>> {
        match (&self.from, &self.from_file) {
            (Some(path), None) => Ok(Source::Backup(path)),
            (None, Some(path)) => Ok(Source::File(path)),
            _ => Err(CommandError("give --from REPORT or --from-file FILE".to_owned()).into()),
        }
    }

    /// Plan the restore against the current backup, before any capture or
    /// radio work.
    ///
    /// Returns an error when the configuration to restore does not load, or
    /// is not a complete standard backup with the current one's identity or a
    /// `.d750` file for its radio type.
    fn prepare(&self, current: &Snapshot) -> AppResult<RestorePlan> {
        Ok(match self.source()? {
            Source::Backup(path) => RestorePlan::new(
                current.configuration(),
                Snapshot::load(path)?.configuration(),
            )?,
            Source::File(path) => {
                RestorePlan::from_d750(current.configuration(), &read_d750(path)?)?
            }
        })
    }
}

impl RestoreRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, RestoreCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp restore apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &RestoreRequest) -> Option<AppResult<()>> {
    match &request.command {
        RestoreCommand::Preview(selection) => Some(preview(selection)),
        RestoreCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &RestoreRequest,
) -> AppResult<()> {
    match &request.command {
        RestoreCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        RestoreCommand::Preview(selection) => preview(selection),
    }
}

async fn run_apply(endpoint: &SerialCandidate, baud: u32, request: &ApplyRequest) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::Restore)?;
    let current = Snapshot::load_for_usb_write(&request.selection.backup)?;
    let plan = request.selection.prepare(&current)?;
    print_plan(&plan);
    if plan.replacements().is_empty() {
        return Err(CommandError(
            "the radio's backup already holds the restored configuration; nothing was opened"
                .to_owned(),
        )
        .into());
    }
    let pages = PagePlan::pages(
        PlanKind::Restore,
        current.identity(),
        plan.replacements(),
        description(&plan, &request.selection.source()?),
    );
    page_apply::run_usb(
        endpoint,
        baud,
        &pages,
        request.output.as_deref(),
        &request.selection.backup,
    )
    .await
}

fn preview(selection: &RestoreSelection) -> AppResult<()> {
    let plan = selection.prepare(&Snapshot::load(&selection.backup)?)?;
    print_plan(&plan);
    if plan.replacements().is_empty() {
        output::line(format_args!(
            "The radio's backup already holds the restored configuration; apply would write nothing."
        ));
    } else {
        output::line(format_args!(
            "Preview only; mcp restore apply --apply writes {} pages.",
            plan.replacements().len()
        ));
    }
    Ok(())
}

/// Bytes the plan's replacements change.
fn changed_bytes(plan: &RestorePlan) -> usize {
    plan.replacements()
        .iter()
        .map(|replacement| {
            replacement
                .expected()
                .iter()
                .zip(replacement.replacement())
                .filter(|(before, after)| before != after)
                .count()
        })
        .sum()
}

fn print_plan(plan: &RestorePlan) {
    let pages: Vec<String> = plan
        .replacements()
        .iter()
        .take(LISTED_PAGES)
        .map(|replacement| format!("{:#07X}", replacement.page().address().as_u32()))
        .collect();
    let more = plan.replacements().len().saturating_sub(LISTED_PAGES);
    output::line(format_args!(
        "Restore changes {} bytes in {} pages{}{}.",
        changed_bytes(plan),
        plan.replacements().len(),
        if pages.is_empty() { "" } else { ": " },
        pages.join(", ")
    ));
    if more > 0 {
        output::line(format_args!("  and {more} more pages."));
    }
    for kept in plan.kept() {
        match kept.slot() {
            Some(slot) => output::line(format_args!(
                "Keeps the current {} of slot {}.",
                kept.field().name(),
                slot.index()
            )),
            None => output::line(format_args!("Keeps the current {}.", kept.field().name())),
        }
    }
}

/// The journal description of a restore: the backup or file restored, the
/// pages and bytes written, and the lifecycle settings kept.
fn description(plan: &RestorePlan, source: &Source<'_>) -> Map<String, Value> {
    let (kind, from) = match source {
        Source::Backup(path) => ("backup", path),
        Source::File(path) => ("d750", path),
    };
    let kept: Vec<Value> = plan
        .kept()
        .iter()
        .map(|kept| {
            json!({
                "field": kept.field().name(),
                "slot": kept.slot().map(kenwood_tmd750::SlotIndex::index),
            })
        })
        .collect();
    let mut description = Map::new();
    let _previous = description.insert(
        "restore".to_owned(),
        json!({
            "source": kind,
            "from": from.display().to_string(),
            "pages": plan.replacements().len(),
            "changed_bytes": changed_bytes(plan),
            "kept": kept,
        }),
    );
    description
}

#[cfg(test)]
mod tests;
