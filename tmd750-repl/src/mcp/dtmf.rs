//! The DTMF and `EchoLink` memories: an offline listing and preview, and one
//! channel write over one MCP session.
//!
//! An edit stores or clears one channel of one memory, starting from the
//! backup's memories; `apply` writes the changed pages through the shared
//! page workflow, which compares each whole page before its write and reads
//! it back.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kenwood_tmd750::memory::dtmf::{
    DtmfChannel, DtmfMemories, DtmfMemoryEntry, DtmfMemoryKind, EchoLinkMemoryEntry,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{PageReplacement, Region};
use serde_json::{Map, Value, json};

use super::page_apply::{self, PagePlan, PlanKind};
use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// The image spans of the pages holding the memories: the DTMF codes, the
/// DTMF names, and the `EchoLink` memory.
const PAGES: [(u32, u32); 3] = [(0x38, 0x100), (0x100, 0x1A0), (0x200, 0x300)];

/// DTMF subcommand: an offline listing or preview, or one guarded live
/// write.
#[derive(Debug, Parser)]
pub(crate) struct DtmfRequest {
    #[command(subcommand)]
    command: DtmfCommand,
}

#[derive(Debug, Subcommand)]
enum DtmfCommand {
    /// Print the backup's DTMF and `EchoLink` memories; never writes.
    List(ListRequest),
    /// Show one channel before and after an edit; never writes.
    Preview(EditRequest),
    /// Write one edited channel, comparing and reading back each changed
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
    /// The memory to edit.
    #[arg(long, value_enum)]
    memory: Memory,
    /// Channel, 0 to 9, as the radio numbers it.
    #[arg(long, value_name = "CHANNEL")]
    channel: u8,
    /// Name: at most 16 printable ASCII characters (8 for `--memory
    /// echolink`); omitted stores none.
    #[arg(long, conflicts_with = "clear", allow_hyphen_values = true)]
    name: Option<String>,
    /// Code: at most 16 of `0`-`9`, `A`-`D`, `*`, `#` and space for a pause
    /// (8 for `--memory echolink`); omitted stores none.
    #[arg(long, conflicts_with = "clear")]
    code: Option<String>,
    /// Clear the channel instead.
    #[arg(long)]
    clear: bool,
}

/// Arguments of `mcp dtmf apply`.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    edit: EditRequest,
    /// Required. The edited channel stays on the radio; nothing is rolled
    /// back.
    #[arg(long, required = true)]
    apply: bool,
    /// New private capture directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

/// A memory as the command line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Memory {
    /// The DTMF memory.
    Dtmf,
    /// The `EchoLink` memory.
    #[value(name = "echolink")]
    EchoLink,
}

impl Memory {
    const fn library(self) -> DtmfMemoryKind {
        match self {
            Self::Dtmf => DtmfMemoryKind::Dtmf,
            Self::EchoLink => DtmfMemoryKind::EchoLink,
        }
    }
}

/// The journal spelling of a memory, as the command line names it.
const fn label(kind: DtmfMemoryKind) -> &'static str {
    match kind {
        DtmfMemoryKind::Dtmf => "dtmf",
        DtmfMemoryKind::EchoLink => "echolink",
    }
}

/// A stored channel's name and code.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stored {
    name: String,
    code: String,
}

/// One planned edit: the channel before and after, and the page writes.
struct Prepared {
    kind: DtmfMemoryKind,
    channel: DtmfChannel,
    before: Option<Stored>,
    after: Option<Stored>,
    replacements: Vec<PageReplacement>,
}

/// The memories decoded from a backup that captured all three pages.
fn captured(snapshot: &Snapshot) -> AppResult<(DtmfMemories, &kenwood_tmd750::MemoryImage)> {
    let [codes, names, echolink] = PAGES.map(|(start, end)| Region::new(start, end));
    let image = snapshot.image_covering(codes?)?;
    for region in [names?, echolink?] {
        let _covered = snapshot.image_covering(region)?;
    }
    Ok((DtmfMemories::read(image)?, image))
}

/// The channel's name and code, `None` when unused.
fn stored(memories: &DtmfMemories, kind: DtmfMemoryKind, channel: DtmfChannel) -> Option<Stored> {
    let fields = |name: &str, code: &str| Stored {
        name: name.to_owned(),
        code: code.to_owned(),
    };
    match kind {
        DtmfMemoryKind::Dtmf => memories
            .dtmf(channel)
            .map(|entry| fields(entry.name(), entry.code())),
        DtmfMemoryKind::EchoLink => memories
            .echolink(channel)
            .map(|entry| fields(entry.name(), entry.code())),
    }
}

impl EditRequest {
    /// Plan the edit against the backup, before any capture or radio work.
    ///
    /// Returns an error when neither `--name`/`--code` nor `--clear` is
    /// given, the channel is above 9, or the library refuses the entry.
    fn prepare(&self, snapshot: &Snapshot) -> AppResult<Prepared> {
        let entry = match (self.name.as_deref(), self.code.as_deref(), self.clear) {
            (None, None, true) => None,
            (name @ Some(_), code, false) | (name, code @ Some(_), false) => {
                Some((name.unwrap_or_default(), code.unwrap_or_default()))
            }
            _ => {
                return Err(
                    CommandError("give --name, --code or both, or --clear".to_owned()).into(),
                );
            }
        };
        let channel = DtmfChannel::new(self.channel)?;
        let kind = self.memory.library();
        let (mut memories, image) = captured(snapshot)?;
        let before = stored(&memories, kind, channel);
        match kind {
            DtmfMemoryKind::Dtmf => memories.set_dtmf(
                channel,
                entry
                    .map(|(name, code)| DtmfMemoryEntry::new(name, code))
                    .transpose()?,
            ),
            DtmfMemoryKind::EchoLink => memories.set_echolink(
                channel,
                entry
                    .map(|(name, code)| EchoLinkMemoryEntry::new(name, code))
                    .transpose()?,
            ),
        }
        Ok(Prepared {
            kind,
            channel,
            before,
            after: stored(&memories, kind, channel),
            replacements: memories.page_replacements(image)?,
        })
    }
}

impl DtmfRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, DtmfCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp dtmf apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &DtmfRequest) -> Option<AppResult<()>> {
    match &request.command {
        DtmfCommand::List(list) => Some(print_list(list)),
        DtmfCommand::Preview(edit) => Some(preview(edit)),
        DtmfCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &DtmfRequest,
) -> AppResult<()> {
    match &request.command {
        DtmfCommand::Apply(request) => run_apply(endpoint, baud, request).await,
        DtmfCommand::List(list) => print_list(list),
        DtmfCommand::Preview(edit) => preview(edit),
    }
}

async fn run_apply(endpoint: &SerialCandidate, baud: u32, request: &ApplyRequest) -> AppResult<()> {
    page_apply::admit_usb(endpoint, baud, PlanKind::Dtmf)?;
    let snapshot = Snapshot::load_for_usb_write(&request.edit.backup)?;
    let prepared = request.edit.prepare(&snapshot)?;
    print_edit(&prepared);
    if prepared.replacements.is_empty() {
        return Err(CommandError(
            "the backup already holds this channel; nothing was opened".to_owned(),
        )
        .into());
    }
    let plan = PagePlan::pages(
        PlanKind::Dtmf,
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
    let (memories, _image) = captured(&Snapshot::load(&request.backup)?)?;
    for kind in DtmfMemoryKind::ALL {
        output::line(format_args!("{kind}:"));
        for channel in DtmfChannel::ALL {
            output::line(format_args!(
                "  {channel} {}",
                show(stored(&memories, kind, channel).as_ref())
            ));
        }
    }
    Ok(())
}

fn preview(edit: &EditRequest) -> AppResult<()> {
    let prepared = edit.prepare(&Snapshot::load(&edit.backup)?)?;
    print_edit(&prepared);
    if prepared.replacements.is_empty() {
        output::line(format_args!(
            "The backup already holds this channel; apply would write nothing."
        ));
    } else {
        let pages: Vec<String> = prepared
            .replacements
            .iter()
            .map(|replacement| format!("{:#07X}", replacement.page().address().as_u32()))
            .collect();
        output::line(format_args!(
            "Preview only; mcp dtmf apply --apply writes pages {}.",
            pages.join(", ")
        ));
    }
    Ok(())
}

/// A channel as `list` and the edit lines print it.
fn show(stored: Option<&Stored>) -> String {
    stored.map_or_else(
        || "unused".to_owned(),
        |stored| format!("code {:?}, name {:?}", stored.code, stored.name),
    )
}

fn print_edit(prepared: &Prepared) {
    let (kind, channel) = (prepared.kind, prepared.channel);
    output::line(format_args!(
        "{kind} {channel} before: {}",
        show(prepared.before.as_ref())
    ));
    output::line(format_args!(
        "{kind} {channel} after:  {}",
        show(prepared.after.as_ref())
    ));
}

/// The journal description of an edit: the memory, the channel and both
/// states.
fn description(prepared: &Prepared) -> Map<String, Value> {
    let entry = |stored: Option<&Stored>| {
        stored.map_or(
            Value::Null,
            |stored| json!({"name": stored.name, "code": stored.code}),
        )
    };
    let mut description = Map::new();
    let _previous = description.insert(
        "dtmf".to_owned(),
        json!({
            "memory": label(prepared.kind),
            "channel": prepared.channel.index(),
            "before": entry(prepared.before.as_ref()),
            "after": entry(prepared.after.as_ref()),
        }),
    );
    description
}

#[cfg(test)]
mod tests;
