//! Generic keyboard menu inspection, preview, and guarded live dispatch.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{
    DecodedFieldValue, Endian, FieldCodec, MCP_D750_MENU_FIELDS, MenuField, MenuWritePolicy,
    PatchPlanner, StringEncoding, is_supported_schema_target, menu_field,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    MenuAssignment, MenuFieldSnapshot, MenuUpdatePlan, RadioModel, Region, ScopedMenuField,
    SlotIndex,
};
use serde::Serialize;

use super::snapshot::Snapshot;
use super::{IdentityEvidence, capture, write_report};
use crate::{AppResult, CommandError, output};

/// Discover menu fields offline or explicitly select a guarded live update.
#[derive(Debug, Parser)]
pub(crate) struct MenuRequest {
    #[command(subcommand)]
    command: MenuCommand,
}

#[derive(Debug, Subcommand)]
enum MenuCommand {
    /// List registered fields, scopes, storage bounds, and ordinary-write policies.
    List {
        /// Restrict to a registry group, such as radio, gps, aprs, dv, ipnet, or pm.
        #[arg(long)]
        group: Option<String>,
    },
    /// Describe one field, including public choice labels and raw stored values.
    Describe { field: String },
    /// Decode captured stored bytes without opening a radio.
    Show(Selection),
    /// Preview exact scalar changes locally; never apply them to a radio.
    Preview(PreviewRequest),
    /// Apply an explicitly approved ordinary menu change with complete-page guards.
    Apply(ApplyRequest),
}

#[derive(Debug, Args)]
struct Selection {
    /// Successful standard configuration-backup report.json, not a raw image.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Explicit PM slot for per-slot fields; zero means PM Off. Omit for globals.
    #[arg(long, value_parser = parse_slot, value_name = "0..5")]
    slot: Option<SlotIndex>,
    /// Registered field key from mcp menu list.
    field: String,
}

#[derive(Debug, Args)]
struct PreviewRequest {
    #[command(flatten)]
    selection: Selection,
    /// Exact scalar value; quote spaces. Numeric values are stored integers.
    #[arg(allow_hyphen_values = true)]
    value: String,
    /// New private JSON preview file; existing files are never overwritten.
    #[arg(long, value_name = "NEW_JSON")]
    output: Option<PathBuf>,
}

/// One CLI assignment adapted to the library's reusable batch plan.
#[derive(Debug, Args)]
pub(super) struct ApplyRequest {
    #[command(flatten)]
    selection: Selection,
    /// Approve leaving the requested setting in place without automatic rollback.
    #[arg(long, required = true)]
    apply: bool,
    /// Exact scalar value, interpreted through the selected field's storage codec.
    #[arg(allow_hyphen_values = true)]
    value: String,
    /// New private evidence directory; existing directories are never overwritten.
    #[arg(long, value_name = "NEW_DIR")]
    output: Option<PathBuf>,
}

impl MenuRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if let MenuCommand::Apply(request) = &self.command {
            request.validate_options()?;
            if !explicit_port {
                return Err(CommandError(
                    "mcp menu apply requires an explicit --port before mcp".to_owned(),
                )
                .into());
            }
        }
        Ok(())
    }
}

impl ApplyRequest {
    pub(super) fn source_backup(&self) -> &Path {
        &self.selection.backup
    }

    pub(super) fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    pub(super) fn validate_options(&self) -> AppResult<()> {
        self.assignment().map(|_assignment| ())
    }

    fn assignment(&self) -> AppResult<MenuAssignment> {
        if !self.apply {
            return Err(CommandError("mcp menu apply requires explicit --apply".to_owned()).into());
        }
        Ok(MenuAssignment::new(
            &self.selection.field,
            self.selection.slot,
            &self.value,
        )?)
    }

    /// Bind a typed request to the strict source backup before capture or USB work.
    pub(super) fn prepare(&self) -> AppResult<MenuUpdatePlan> {
        let assignment = self.assignment()?;
        let snapshot = Snapshot::load(self.source_backup())?;
        Ok(MenuUpdatePlan::new(
            &snapshot.identity,
            &snapshot.menu_snapshot()?,
            vec![assignment],
        )?)
    }
}

pub(super) fn run_offline(request: &MenuRequest) -> Option<AppResult<()>> {
    match request.command {
        MenuCommand::Apply(_) => None,
        _ => Some(run(request)),
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &MenuRequest,
) -> AppResult<()> {
    match &request.command {
        MenuCommand::Apply(request) => super::menu_apply::run(endpoint, baud, request).await,
        _ => run(request),
    }
}

fn run(request: &MenuRequest) -> AppResult<()> {
    match &request.command {
        MenuCommand::List { group } => list(group.as_deref()),
        MenuCommand::Describe { field } => describe(field, &mut std::io::stdout().lock()),
        MenuCommand::Show(selection) => {
            let (snapshot, menu, field) = selection.load()?;
            let value = menu.value(field)?;
            print_source(&snapshot);
            output::line(format_args!(
                "{}; scope {}; policy {}; stored value {}.",
                field.field().descriptor.name,
                scope_label(field.slot()),
                policy_label(field.field().write_policy()),
                serde_json::to_string(&Value::from(&value))?
            ));
            Ok(())
        }
        MenuCommand::Preview(request) => preview(request),
        MenuCommand::Apply(_) => Err(CommandError(
            "mcp menu apply requires its dedicated live workflow".to_owned(),
        )
        .into()),
    }
}

/// Structured output must not pass through prose wrapping or timestamp prefixes.
fn describe(field: &str, writer: &mut impl std::io::Write) -> AppResult<()> {
    Ok(write_report(writer, &Description::new(resolve(field)?))?)
}

fn parse_slot(text: &str) -> Result<SlotIndex, String> {
    let value = text
        .parse::<u8>()
        .map_err(|_| "slot must be an integer from 0 through 5".to_owned())?;
    SlotIndex::new(value).map_err(|error| error.to_string())
}

fn resolve(name: &str) -> AppResult<&'static MenuField> {
    menu_field(name)
        .ok_or_else(|| CommandError("unknown menu field; use mcp menu list".to_owned()).into())
}

impl Selection {
    fn load(&self) -> AppResult<(Snapshot, MenuFieldSnapshot, ScopedMenuField<'static>)> {
        let selection = ScopedMenuField::new(resolve(&self.field)?, self.slot)?;
        let snapshot = Snapshot::load(&self.backup)?;
        validate_layout(&snapshot)?;
        let menu = snapshot.menu_snapshot()?;
        Ok((snapshot, menu, selection))
    }
}

fn validate_layout(snapshot: &Snapshot) -> AppResult<()> {
    let identity = &snapshot.identity;
    if !(is_supported_schema_target(identity.model, &identity.firmware)
        || (identity.model == RadioModel::TmD750
            && identity.firmware.as_str() == "1.02"
            && identity.radio_type.as_str() == "K,2,1"))
    {
        return Err(CommandError(
            "backup identity is outside supported menu-layout interpretation".to_owned(),
        )
        .into());
    }
    if snapshot.captured_bytes(Region::new(10, 11)?)? != [0] {
        return Err(CommandError(
            "menu interpretation requires captured memory-format byte zero".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn list(group: Option<&str>) -> AppResult<()> {
    let fields: Vec<_> = MCP_D750_MENU_FIELDS
        .iter()
        .filter(|field| group.is_none_or(|group| field.menu.eq_ignore_ascii_case(group)))
        .collect();
    if fields.is_empty() {
        return Err(CommandError(
            "unknown menu group; use radio, gps, aprs, dv, ipnet, or pm".to_owned(),
        )
        .into());
    }
    for field in fields {
        let description = Description::new(field);
        output::line(format_args!(
            "{}: {}; {}; policy {}.",
            description.field, description.scope, description.storage, description.write_policy
        ));
    }
    output::line(format_args!(
        "Values are stored representations, not inferred display units. Policy is software admission, not hardware qualification; apply checks identity and complete state guards separately."
    ));
    Ok(())
}

#[derive(Debug, Serialize)]
struct Description {
    field: &'static str,
    group: &'static str,
    scope: &'static str,
    storage: String,
    choices: Vec<Choice>,
    allowed_values: &'static [u64],
    write_policy: &'static str,
    interpretation: &'static str,
}

#[derive(Debug, Serialize)]
struct Choice {
    raw: u64,
    label: Option<&'static str>,
}

impl Description {
    fn new(field: &MenuField) -> Self {
        Self {
            field: field.descriptor.name,
            group: field.menu,
            scope: if field.descriptor.is_per_slot() {
                "requires --slot 0..5; 0 is PM Off"
            } else {
                "global; omit --slot"
            },
            storage: storage_description(field.descriptor.codec),
            choices: field
                .options
                .iter()
                .map(|option| Choice {
                    raw: option.raw,
                    label: option.label,
                })
                .collect(),
            allowed_values: field.allowed_values,
            write_policy: policy_label(field.write_policy()),
            interpretation: "Raw storage, not inferred display units or hardware qualification. Supplemental ordinary-value domains also apply.",
        }
    }
}

fn storage_description(codec: FieldCodec) -> String {
    match codec {
        FieldCodec::Byte { min, max } => format!("unsigned byte; stored range {min}..={max}"),
        FieldCodec::Bool => "boolean byte; true/false, on/off, yes/no, 1/0".to_owned(),
        FieldCodec::BitBool { mask } => {
            format!("boolean bit; mask 0x{mask:02X}; true/false, on/off, yes/no, 1/0")
        }
        FieldCodec::BitField {
            mask,
            shift,
            min,
            max,
        } => format!("unsigned bits; mask 0x{mask:02X}, shift {shift}; stored range {min}..={max}"),
        FieldCodec::FixedString {
            len,
            encoding,
            padding,
        } => {
            let encoding = match encoding {
                StringEncoding::Utf8 => "UTF-8",
                StringEncoding::MemoryMap => "printable ASCII",
            };
            format!("exact {encoding} text; maximum {len} encoded bytes; padding 0x{padding:02X}")
        }
        FieldCodec::Unsigned {
            width,
            endian,
            min,
            max,
        } => format!(
            "unsigned {width}-byte {}; stored range {min}..={max}",
            endian_label(endian)
        ),
        FieldCodec::Signed {
            width,
            endian,
            min,
            max,
        } => format!(
            "signed {width}-byte {}; stored range {min}..={max}",
            endian_label(endian)
        ),
        FieldCodec::Bytes { len } => format!("binary; {len} bytes; no scalar preview/apply"),
    }
}

const fn endian_label(endian: Endian) -> &'static str {
    match endian {
        Endian::Little => "little-endian",
        Endian::Big => "big-endian",
    }
}

const fn policy_label(policy: MenuWritePolicy) -> &'static str {
    match policy {
        MenuWritePolicy::Ordinary => "ordinary",
        MenuWritePolicy::UnresolvedDomain => {
            "unresolved_domain; not applicable to ordinary updates"
        }
        MenuWritePolicy::LifecycleRequired => {
            "lifecycle_required; not applicable to ordinary updates"
        }
        MenuWritePolicy::Binary => "binary; not applicable to ordinary updates",
        _ => "unsupported_policy; not applicable to ordinary updates",
    }
}

fn scope_label(slot: Option<SlotIndex>) -> String {
    slot.map_or_else(
        || "global".to_owned(),
        |slot| format!("PM slot {}", slot.index()),
    )
}

fn print_source(snapshot: &Snapshot) {
    output::line(format_args!(
        "Captured identity: {} firmware {}, type {}; format-zero software layout, not current radio state or hardware qualification.",
        snapshot.identity.model, snapshot.identity.firmware, snapshot.identity.radio_type
    ));
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(super) enum Value {
    Unsigned(u64),
    Signed(i64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
}

impl From<&DecodedFieldValue> for Value {
    fn from(value: &DecodedFieldValue) -> Self {
        match value {
            DecodedFieldValue::Unsigned(value) => Self::Unsigned(*value),
            DecodedFieldValue::Signed(value) => Self::Signed(*value),
            DecodedFieldValue::Bool(value) => Self::Bool(*value),
            DecodedFieldValue::Text(value) => Self::Text(value.clone()),
            DecodedFieldValue::Bytes(value) => Self::Bytes(value.clone()),
        }
    }
}

#[derive(Debug, Serialize)]
struct Preview {
    format_version: u8,
    operation: &'static str,
    offline_only: bool,
    radio_applied: bool,
    source_backup: PathBuf,
    identity: IdentityEvidence,
    field: &'static str,
    slot: Option<u8>,
    write_policy: &'static str,
    before: Value,
    after: Value,
    changed_bytes: usize,
    pages: Vec<PagePreview>,
}

#[derive(Debug, Serialize)]
struct PagePreview {
    address: u32,
    length: usize,
    bytes: Vec<BytePreview>,
}

#[derive(Debug, Serialize)]
struct BytePreview {
    offset: u8,
    mask: u8,
    before: u8,
    after: u8,
}

impl PreviewRequest {
    fn prepare(&self) -> AppResult<Preview> {
        let (snapshot, menu, selection) = self.selection.load()?;
        let field = selection.field();
        let before = menu.value(selection)?;
        let desired = field.parse_value(&self.value)?;
        if field.write_policy() == MenuWritePolicy::Ordinary {
            field.validate_ordinary_value(desired.as_field_value())?;
        }
        let mut planner = PatchPlanner::new();
        let _planned = planner.set_menu(field, selection.slot(), desired.as_field_value())?;
        let patches = planner.finish()?;
        let _coverage = menu.plan_exchanges(&patches)?;
        let mut changed_bytes = 0;
        let mut pages = Vec::new();
        for patch in patches.pages() {
            let captured = menu
                .page(patch.page())
                .ok_or("preview lacks its complete captured page")?;
            let mut bytes = Vec::new();
            for byte in patch.bytes() {
                let before = *captured
                    .get(usize::from(byte.offset()))
                    .ok_or("preview byte exceeds its canonical page")?;
                let after = (before & !byte.mask()) | byte.value();
                changed_bytes += usize::from(before != after);
                bytes.push(BytePreview {
                    offset: byte.offset(),
                    mask: byte.mask(),
                    before,
                    after,
                });
            }
            pages.push(PagePreview {
                address: patch.page().address().as_u32(),
                length: patch.page().len(),
                bytes,
            });
        }
        Ok(Preview {
            format_version: 1,
            operation: "offline_menu_preview",
            offline_only: true,
            radio_applied: false,
            source_backup: self.selection.backup.clone(),
            identity: IdentityEvidence::from(&snapshot.identity),
            field: field.descriptor.name,
            slot: selection.slot().map(SlotIndex::index),
            write_policy: policy_label(field.write_policy()),
            before: Value::from(&before),
            after: Value::from(&desired),
            changed_bytes,
            pages,
        })
    }
}

fn preview(request: &PreviewRequest) -> AppResult<()> {
    let prepared = request.prepare()?;
    if let Some(path) = &request.output {
        let mut writer = capture::create_private_file(path)?;
        write_report(&mut writer, &prepared)?;
        writer.sync_all()?;
        output::line(format_args!(
            "Offline menu preview saved: {}.",
            path.display()
        ));
    }
    output::line(format_args!(
        "{}; policy {}; before {}; after {}; {} changed bytes.",
        prepared.field,
        prepared.write_policy,
        serde_json::to_string(&prepared.before)?,
        serde_json::to_string(&prepared.after)?,
        prepared.changed_bytes
    ));
    output::line(format_args!(
        "Offline preview only. Backup unchanged; nothing applied to a radio. Field policy does not establish live guards or authorize apply."
    ));
    Ok(())
}

#[cfg(test)]
mod tests;
