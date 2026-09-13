//! Offline text inspection and explicitly approved, field-scoped updates.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{
    StringEncoding, TextImage, TextLayoutQualification, TextMetadata, TextPreview, TextScope,
    TextScopeKind, TextSetting, menu_field,
};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{Identity, MemoryImage, SlotIndex};
use serde::Serialize;

use super::snapshot::Snapshot;
use super::{IdentityEvidence, write_report};
use crate::{AppResult, CommandError, capture, output};

/// Inspect text offline or select a separately bounded typed text setter.
#[derive(Debug, Parser)]
pub(crate) struct TextRequest {
    #[command(subcommand)]
    command: TextCommand,
}

#[derive(Debug, Subcommand)]
enum TextCommand {
    /// List supported setting keys, scopes, and storage limits without a backup.
    List,
    /// Show captured text using an explicitly selected software-layout policy.
    Show(Selection),
    /// Preview a text change without altering the backup or applying it to a radio.
    Preview(PreviewRequest),
    /// Set PM1's name or PM-Off MY1, then verify across MCP exit/re-entry.
    Set(super::text_set::SetRequest),
}

impl TextRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if let TextCommand::Set(request) = &self.command {
            request.validate_options()?;
            if !explicit_port {
                return Err(Box::new(CommandError(
                    "mcp text set requires an explicit --port before mcp".to_owned(),
                )));
            }
        }
        Ok(())
    }
}

/// Execute offline commands without enumerating or opening any endpoint.
pub(super) fn run_offline(request: &TextRequest) -> Option<AppResult<()>> {
    match request.command {
        TextCommand::Set(_) => None,
        _ => Some(run(request)),
    }
}

/// Dispatch an explicitly selected connection only to the bounded setter.
pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &TextRequest,
) -> AppResult<()> {
    match &request.command {
        TextCommand::Set(request) => super::text_set::run(endpoint, baud, request).await,
        _ => run(request),
    }
}

#[derive(Debug, Args)]
struct Selection {
    /// Successful configuration-backup report.json; never a raw or partial image.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,

    /// Explicitly interpret unqualified firmware using the software layout.
    ///
    /// Required for firmware 1.02. This does not enable any radio writes.
    #[arg(long)]
    interpret_unqualified: bool,

    /// Zero-based programmable-memory slot, required only for per-slot settings.
    #[arg(long, value_parser = parse_slot, value_name = "0..5")]
    slot: Option<SlotIndex>,

    /// Stable setting key from `mcp text list`.
    setting: TextSetting,
}

#[derive(Debug, Args)]
struct PreviewRequest {
    #[command(flatten)]
    selection: Selection,

    /// Exact replacement text; quote spaces and use an empty quoted value to clear.
    text: String,

    /// New private JSON patch file; never overwrite an existing file.
    #[arg(long, value_name = "NEW_PATCH.json")]
    output: Option<PathBuf>,
}

fn parse_slot(value: &str) -> Result<SlotIndex, String> {
    let index = value
        .parse::<u8>()
        .map_err(|_| "slot must be an integer from 0 through 5".to_owned())?;
    SlotIndex::new(index).map_err(|error| error.to_string())
}

impl Selection {
    fn scope(&self, metadata: TextMetadata) -> AppResult<TextScope> {
        match (metadata.scope, self.slot) {
            (TextScopeKind::Global, None) => Ok(TextScope::Global),
            (TextScopeKind::PerSlot, Some(slot)) => Ok(TextScope::Slot(slot)),
            (TextScopeKind::Global, Some(_)) => Err(Box::new(CommandError(format!(
                "{} is global; omit --slot",
                self.setting
            )))),
            (TextScopeKind::PerSlot, None) => Err(Box::new(CommandError(format!(
                "{} requires an explicit --slot from 0 through 5",
                self.setting
            )))),
        }
    }

    fn view<'a>(&self, snapshot: &'a Snapshot) -> AppResult<(TextImage<'a>, TextScope)> {
        let metadata = self.setting.metadata()?;
        let scope = self.scope(metadata)?;
        let field = menu_field(metadata.field_name).ok_or_else(|| {
            CommandError(format!("no text storage descriptor for {}", self.setting))
        })?;
        let image = snapshot
            .image_for(&field.descriptor, self.slot)
            .map_err(|error| setting_error(self.setting, metadata, error.as_ref()))?;
        let view = if self.interpret_unqualified {
            TextImage::interpret_unqualified(image, &snapshot.identity.firmware)
        } else {
            TextImage::new(image, &snapshot.identity.firmware)?
        };
        Ok((view, scope))
    }
}

/// The offline implementation never opens a radio, even if misrouted.
fn run(request: &TextRequest) -> AppResult<()> {
    match &request.command {
        TextCommand::List => list(),
        TextCommand::Show(selection) => {
            let snapshot = Snapshot::load(&selection.backup)?;
            let (view, scope) = selection.view(&snapshot)?;
            let metadata = selection.setting.metadata()?;
            let value = view
                .read(selection.setting, scope)
                .map_err(|error| setting_error(selection.setting, metadata, &error))?;
            print_provenance(&snapshot.identity, view.qualification());
            output::line(format_args!(
                "{}{}: {}",
                selection.setting,
                scope_label(scope),
                serde_json::to_string(&value)?
            ));
            Ok(())
        }
        TextCommand::Preview(request) => preview(request),
        TextCommand::Set(_) => Err(Box::new(CommandError(
            "mcp text set requires its dedicated live workflow".to_owned(),
        ))),
    }
}

fn list() -> AppResult<()> {
    output::line(format_args!(
        "Offline text settings. Limits are encoded bytes, not character counts."
    ));
    for setting in TextSetting::all() {
        let metadata = setting.metadata()?;
        let scope = match metadata.scope {
            TextScopeKind::Global => "global; omit --slot",
            TextScopeKind::PerSlot => "requires --slot 0..5",
        };
        let encoding = match metadata.encoding {
            StringEncoding::Utf8 => "UTF-8",
            StringEncoding::MemoryMap => "printable ASCII",
        };
        output::line(format_args!(
            "{setting}: {scope}; maximum {} bytes; {encoding}.",
            metadata.max_bytes
        ));
    }
    output::line(format_args!(
        "This legacy text view requires --interpret-unqualified on firmware 1.02. Dedicated pm-name-1 and PM-Off MY1 setters: mcp text set --help. General menu discovery, preview, and ordinary updates: mcp menu --help."
    ));
    Ok(())
}

fn preview(request: &PreviewRequest) -> AppResult<()> {
    let selection = &request.selection;
    let snapshot = Snapshot::load(&selection.backup)?;
    let (view, scope) = selection.view(&snapshot)?;
    let metadata = selection.setting.metadata()?;
    let preview = view
        .preview(selection.setting, scope, &request.text)
        .map_err(|error| setting_error(selection.setting, metadata, &error))?;
    let field = menu_field(metadata.field_name).ok_or_else(|| {
        CommandError(format!(
            "no text storage descriptor for {}",
            selection.setting
        ))
    })?;
    let image = snapshot
        .image_for(&field.descriptor, selection.slot)
        .map_err(|error| setting_error(selection.setting, metadata, error.as_ref()))?;
    let artifact = PreviewArtifact::new(&selection.backup, &snapshot.identity, image, &preview)?;
    if let Some(path) = &request.output {
        save_preview(path, &artifact)?;
        output::line(format_args!("Offline patch saved: {}.", path.display()));
    }
    print_provenance(&snapshot.identity, preview.qualification());
    output::line(format_args!(
        "{}{}; before: {}; after: {}.",
        preview.setting(),
        scope_label(preview.scope()),
        serde_json::to_string(preview.before())?,
        serde_json::to_string(preview.after())?
    ));
    output::line(format_args!(
        "Offline preview only: {} page patches. Backup unchanged; nothing applied to a radio.",
        artifact.pages.len()
    ));
    Ok(())
}

fn setting_error(
    setting: TextSetting,
    metadata: TextMetadata,
    error: &dyn std::error::Error,
) -> CommandError {
    CommandError(
        error
            .to_string()
            .replace(metadata.field_name, setting.key()),
    )
}

fn print_provenance(identity: &Identity, qualification: TextLayoutQualification) {
    output::line(format_args!(
        "Backup identity: {}; firmware {}; type {}.",
        identity.model, identity.firmware, identity.radio_type
    ));
    output::line(format_args!(
        "{}",
        Qualification::from(qualification).description()
    ));
}

fn scope_label(scope: TextScope) -> String {
    match scope {
        TextScope::Global => " (global)".to_owned(),
        TextScope::Slot(slot) => format!(" (slot {})", slot.index()),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Qualification {
    RegistryTargetMatched,
    UnqualifiedInterpretation,
}

impl Qualification {
    const fn description(&self) -> &'static str {
        match self {
            Self::RegistryTargetMatched => {
                "Registry firmware label matched; hardware schema compatibility is not proved."
            }
            Self::UnqualifiedInterpretation => {
                "Unqualified software-layout interpretation; decoded text and patches are hypotheses, not validated radio settings."
            }
        }
    }
}

impl From<TextLayoutQualification> for Qualification {
    fn from(value: TextLayoutQualification) -> Self {
        match value {
            TextLayoutQualification::RegistryTargetMatched => Self::RegistryTargetMatched,
            TextLayoutQualification::UnqualifiedInterpretation => Self::UnqualifiedInterpretation,
        }
    }
}

#[derive(Debug, Serialize)]
struct PreviewArtifact {
    format_version: u8,
    operation: &'static str,
    offline_only: bool,
    radio_applied: bool,
    source_backup: PathBuf,
    identity: IdentityEvidence,
    qualification: Qualification,
    setting: &'static str,
    slot: Option<u8>,
    before: String,
    after: String,
    pages: Vec<PageEvidence>,
}

#[derive(Debug, Serialize)]
struct PageEvidence {
    address: u32,
    length: usize,
    bytes: Vec<ByteEvidence>,
}

#[derive(Debug, Serialize)]
struct ByteEvidence {
    offset: u8,
    mask: u8,
    value: u8,
    before: u8,
    after: u8,
}

impl PreviewArtifact {
    fn new(
        source: &Path,
        identity: &Identity,
        image: &MemoryImage,
        preview: &TextPreview,
    ) -> AppResult<Self> {
        let pages = preview
            .patches()
            .pages()
            .iter()
            .map(|patch| {
                let bytes = patch
                    .bytes()
                    .iter()
                    .map(|byte| {
                        let address =
                            patch.page().address().as_usize() + usize::from(byte.offset());
                        let before = *image.as_bytes().get(address).ok_or_else(|| {
                            CommandError("planned text byte exceeds the source image".to_owned())
                        })?;
                        Ok(ByteEvidence {
                            offset: byte.offset(),
                            mask: byte.mask(),
                            value: byte.value(),
                            before,
                            after: (before & !byte.mask()) | byte.value(),
                        })
                    })
                    .collect::<AppResult<Vec<_>>>()?;
                Ok(PageEvidence {
                    address: patch.page().address().as_u32(),
                    length: patch.page().len(),
                    bytes,
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        Ok(Self {
            format_version: 1,
            operation: "offline_text_preview",
            offline_only: true,
            radio_applied: false,
            source_backup: source.to_owned(),
            identity: IdentityEvidence::from(identity),
            qualification: preview.qualification().into(),
            setting: preview.setting().key(),
            slot: match preview.scope() {
                TextScope::Global => None,
                TextScope::Slot(slot) => Some(slot.index()),
            },
            before: preview.before().to_owned(),
            after: preview.after().to_owned(),
            pages,
        })
    }
}

fn save_preview(path: &Path, artifact: &PreviewArtifact) -> AppResult<()> {
    let mut file = capture::create_private_file(path)?;
    write_report(&mut file, artifact)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_tmd750::{FirmwareIdentity, RadioModel, RadioType};

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn identity() -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Identity {
            model: RadioModel::TmD750,
            firmware: FirmwareIdentity::new("1.02")?,
            radio_type: RadioType::new("K,2,1")?,
        })
    }

    #[test]
    fn parser_retains_text_and_paths_and_rejects_out_of_range_slots() -> TestResult {
        let request = TextRequest::try_parse_from([
            "text",
            "preview",
            "--backup",
            "Capture Case/report.json",
            "--interpret-unqualified",
            "--slot",
            "5",
            "dstar-message-1",
            "Keep  Spaces",
            "--output",
            "Patch Case.json",
        ])?;
        let TextCommand::Preview(request) = request.command else {
            return Err("expected preview".into());
        };
        assert_eq!(request.text, "Keep  Spaces");
        assert_eq!(
            request.selection.backup,
            PathBuf::from("Capture Case/report.json")
        );
        assert_eq!(request.selection.slot.map(SlotIndex::index), Some(5));
        assert_eq!(request.output, Some(PathBuf::from("Patch Case.json")));
        assert!(
            TextRequest::try_parse_from([
                "text",
                "show",
                "--backup",
                "capture.json",
                "--slot",
                "6",
                "dstar-message-1",
            ])
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn global_and_per_slot_scope_require_explicit_correct_selection() -> TestResult {
        let mut selection = Selection {
            backup: PathBuf::from("capture.json"),
            interpret_unqualified: true,
            slot: None,
            setting: TextSetting::DstarMessage1,
        };
        assert!(selection.scope(selection.setting.metadata()?).is_err());
        selection.slot = Some(SlotIndex::new(0)?);
        assert_eq!(
            selection.scope(selection.setting.metadata()?)?,
            TextScope::Slot(SlotIndex::new(0)?)
        );
        selection.setting = TextSetting::PmName1;
        assert!(selection.scope(selection.setting.metadata()?).is_err());
        Ok(())
    }

    #[test]
    fn preview_artifact_preserves_firmware_hypothesis_and_exact_byte_comparison() -> TestResult {
        let identity = identity()?;
        let image = MemoryImage::from_bytes(vec![0; kenwood_tmd750::types::IMAGE_LENGTH])?;
        let view = TextImage::interpret_unqualified(&image, &identity.firmware);
        let preview = view.preview(TextSetting::PmName1, TextScope::Global, "Portable")?;
        let artifact = PreviewArtifact::new(Path::new("backup.json"), &identity, &image, &preview)?;
        let json = serde_json::to_value(&artifact)?;
        assert_eq!(
            json.get("source_backup"),
            Some(&serde_json::json!("backup.json"))
        );
        assert_eq!(
            json.pointer("/identity/firmware"),
            Some(&serde_json::json!("1.02"))
        );
        assert_eq!(
            json.get("qualification"),
            Some(&serde_json::json!("unqualified_interpretation"))
        );
        assert_eq!(json.get("offline_only"), Some(&serde_json::json!(true)));
        assert_eq!(json.get("radio_applied"), Some(&serde_json::json!(false)));
        assert_eq!(json.get("setting"), Some(&serde_json::json!("pm-name-1")));
        assert_eq!(artifact.before, "");
        assert_eq!(artifact.after, "Portable");
        let first = artifact.pages.first().ok_or("missing page")?;
        let byte = first.bytes.first().ok_or("missing byte")?;
        assert_eq!(first.address + u32::from(byte.offset), 323_594);
        assert_eq!(
            (byte.mask, byte.value, byte.before, byte.after),
            (255, b'P', 0, b'P')
        );
        assert_eq!(first.bytes.len(), 16);
        assert_eq!(image.as_bytes().get(323_594), Some(&0));
        Ok(())
    }

    #[test]
    fn output_file_is_exclusive_and_private() -> TestResult {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("patch.json");
        let identity = identity()?;
        let image = MemoryImage::from_bytes(vec![0; kenwood_tmd750::types::IMAGE_LENGTH])?;
        let preview = TextImage::interpret_unqualified(&image, &identity.firmware).preview(
            TextSetting::PmName1,
            TextScope::Global,
            "Portable",
        )?;
        let artifact = PreviewArtifact::new(Path::new("backup.json"), &identity, &image, &preview)?;
        save_preview(&path, &artifact)?;
        let original = std::fs::read(&path)?;
        assert!(save_preview(&path, &artifact).is_err());
        assert_eq!(std::fs::read(&path)?, original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }
        Ok(())
    }

    #[test]
    fn schema_error_names_are_replaced_with_stable_setting_keys() -> TestResult {
        let setting = TextSetting::PmName1;
        let metadata = setting.metadata()?;
        let field = menu_field(metadata.field_name).ok_or("missing text field")?;
        let result = field
            .descriptor
            .encode(kenwood_tmd750::memory::FieldValue::Text(
                "12345678901234567",
            ));
        let Err(error) = result else {
            return Err("oversized text must fail codec validation".into());
        };
        let message = setting_error(setting, metadata, &error).to_string();
        assert!(message.contains("pm-name-1"));
        assert!(!message.contains(metadata.field_name));
        assert!(message.contains("17"));
        Ok(())
    }

    fn preview_request(backup: &Path, output: &Path) -> PreviewRequest {
        PreviewRequest {
            selection: Selection {
                backup: backup.to_owned(),
                interpret_unqualified: false,
                slot: None,
                setting: TextSetting::PmName1,
            },
            text: "Portable".to_owned(),
            output: Some(output.to_owned()),
        }
    }

    #[test]
    fn firmware_refusal_and_explicit_preview_preserve_source_and_existing_output() -> TestResult {
        let directory = tempfile::tempdir()?;
        let backup = directory.path().join("source.json");
        let output = directory.path().join("patch.json");
        write_report(
            &mut capture::create_private_file(&backup)?,
            &super::super::snapshot::tests::fixture(),
        )?;
        let original = std::fs::read(&backup)?;
        let mut request = preview_request(&backup, &output);
        let refusal = preview(&request);
        assert!(
            refusal.is_err(),
            "firmware 1.02 must require explicit interpretation"
        );
        assert!(!output.exists(), "refusal must not create a patch file");

        request.selection.interpret_unqualified = true;
        preview(&request)?;
        let patch_bytes = std::fs::read(&output)?;
        let patch: serde_json::Value = serde_json::from_slice(&patch_bytes)?;
        assert_eq!(
            patch.get("qualification"),
            Some(&serde_json::json!("unqualified_interpretation"))
        );
        assert_eq!(
            patch.pointer("/identity/firmware"),
            Some(&serde_json::json!("1.02"))
        );
        assert_eq!(
            patch.get("before"),
            Some(&serde_json::json!("B".repeat(16)))
        );
        assert_eq!(patch.get("radio_applied"), Some(&serde_json::json!(false)));

        request.text = "Different".to_owned();
        assert!(
            preview(&request).is_err(),
            "existing patch output must be preserved"
        );
        assert_eq!(std::fs::read(&output)?, patch_bytes);
        assert_eq!(std::fs::read(&backup)?, original);
        Ok(())
    }

    #[test]
    fn invalid_utf8_or_control_source_never_creates_a_preview_artifact() -> TestResult {
        let directory = tempfile::tempdir()?;
        for byte in [0xFF_u8, 0x1B] {
            let mut document = super::super::snapshot::tests::fixture();
            let segments = document
                .pointer_mut("/backup/segments")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or("fixture segments")?;
            let page = segments
                .iter_mut()
                .find(|segment| {
                    segment.get("address").and_then(serde_json::Value::as_u64) == Some(323_584)
                })
                .ok_or("fixture PM-name page")?;
            *page
                .get_mut("data")
                .and_then(serde_json::Value::as_array_mut)
                .and_then(|data| data.get_mut(10))
                .ok_or("fixture PM-name first byte")? = serde_json::json!(byte);
            let backup = directory.path().join(format!("source-{byte}.json"));
            let output = directory.path().join(format!("patch-{byte}.json"));
            write_report(&mut capture::create_private_file(&backup)?, &document)?;
            let mut request = preview_request(&backup, &output);
            request.selection.interpret_unqualified = true;
            let result = preview(&request);
            assert!(
                result.is_err(),
                "invalid source byte {byte:#04x} must be rejected"
            );
            assert!(
                !output.exists(),
                "invalid source must not create a patch file"
            );
        }
        Ok(())
    }
}
