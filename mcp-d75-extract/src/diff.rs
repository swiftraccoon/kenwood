//! Manifest diff report for firmware-release reviews.
//!
//! Two manifests of one radio are compared field by field (direct writes and
//! expanded record fields), record list by record list, and by their
//! declared dimensions. Enum catalogs are compared through the options they
//! attach to a field, never by catalog name: catalog names carry obfuscated
//! declaring classes that change on every program build.

use std::collections::BTreeMap;
use std::fmt;

use crate::address::{Address, Term};
use crate::error::{Result, extract_error};
use crate::manifest::{
    Codec, Domain, EnumCatalog, Manifest, Menu, OffsetLayout, RecordEntry, Role,
};

/// A rendered diff report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    /// Report lines without trailing newlines.
    pub lines: Vec<String>,
    /// Added, removed, and changed items.
    pub differences: usize,
}

impl fmt::Display for DiffReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.lines {
            writeln!(formatter, "{line}")?;
        }
        Ok(())
    }
}

/// One field as the review sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldView {
    address: Address,
    codec: Codec,
    domain: Option<Domain>,
    writable: bool,
    options: Option<Vec<String>>,
}

fn render_terms(terms: &[Term]) -> String {
    terms
        .iter()
        .map(|term| format!("+{}*{}", term.dimension, term.stride))
        .collect::<Vec<_>>()
        .concat()
}

fn render_address(address: &Address) -> String {
    format!("0x{:04X}{}", address.base, render_terms(&address.terms))
}

fn render_codec(codec: &Codec) -> String {
    match codec {
        Codec::Byte { .. } => "byte".to_owned(),
        Codec::Bool { .. } => "bool".to_owned(),
        Codec::RawBytes { length, .. } => format!(
            "raw_bytes len={}",
            length.map_or_else(|| "?".to_owned(), |length| length.to_string())
        ),
        Codec::BitField { bit, width, .. } => format!("bit_field bit={bit} width={width}"),
        Codec::UnsignedLe { width, .. } => format!("unsigned_le width={width}"),
        Codec::SignedLe { width, .. } => format!("signed_le width={width}"),
        Codec::FixedString {
            encoding,
            length,
            padding,
            ..
        } => format!("fixed_string len={length} encoding={encoding} padding={padding}"),
        Codec::ClearRange { length, .. } => format!("clear_range len={length}"),
    }
}

fn render_domain(domain: Option<&Domain>) -> String {
    match domain {
        None => "none".to_owned(),
        Some(Domain::Range { min, max, .. }) => format!("range {min}..={max}"),
        Some(Domain::Choices { allowed_values, .. }) => format!(
            "choices [{}]",
            allowed_values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn render_layout(layout: &OffsetLayout) -> String {
    let stride = layout
        .stride
        .map_or_else(String::new, |stride| format!(" stride {stride}"));
    let overrides = if layout.overrides.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = layout
            .overrides
            .iter()
            .map(|(index, base)| format!("{index}={base}"))
            .collect();
        format!(" overrides {}", pairs.join(","))
    };
    format!(
        "{} base {}{stride}{overrides}{}",
        layout.kind,
        layout.base,
        render_terms(&layout.terms)
    )
}

fn option_signatures(catalog: &EnumCatalog) -> Vec<String> {
    catalog
        .options
        .iter()
        .map(|option| {
            format!(
                "{} {}",
                option.value,
                option
                    .label
                    .clone()
                    .unwrap_or_else(|| option.member.clone())
            )
        })
        .collect()
}

fn render_options(options: Option<&Vec<String>>) -> String {
    options.map_or_else(
        || "none".to_owned(),
        |options| format!("[{}]", options.join(", ")),
    )
}

fn fields_of(menu: &Menu) -> BTreeMap<String, FieldView> {
    let catalogs: BTreeMap<&str, &EnumCatalog> = menu
        .enum_types
        .iter()
        .map(|catalog| (catalog.name.as_str(), catalog))
        .collect();
    let options_for = |codec: &Codec| {
        codec
            .enum_type()
            .and_then(|name| catalogs.get(name))
            .map(|catalog| option_signatures(catalog))
    };
    let mut fields = BTreeMap::new();
    for operation in &menu.operations {
        if operation.role != Role::Field {
            continue;
        }
        let name = operation.name.clone().unwrap_or_else(|| "None".to_owned());
        drop(fields.insert(
            format!("{}.{name}", menu.menu),
            FieldView {
                address: operation.address.clone(),
                codec: operation.codec.clone(),
                domain: operation.domain.clone(),
                writable: operation.writable.unwrap_or(true),
                options: options_for(&operation.codec),
            },
        ));
    }
    for entry in &menu.repeated_records {
        let RecordEntry::Extracted(record) = entry else {
            continue;
        };
        for field in &record.expanded_fields {
            drop(fields.insert(
                format!("{}.{}", menu.menu, field.name),
                FieldView {
                    address: field.address.clone(),
                    codec: field.codec.clone(),
                    domain: field.domain.clone(),
                    writable: field.writable.unwrap_or(true),
                    options: options_for(&field.codec),
                },
            ));
        }
    }
    fields
}

fn records_of(menu: &Menu) -> BTreeMap<String, (u64, String)> {
    menu.repeated_records
        .iter()
        .map(|entry| match entry {
            RecordEntry::Extracted(record) => (
                record.name.clone(),
                (record.count, render_layout(&record.offset_layout)),
            ),
            RecordEntry::Unsupported(record) => (
                record.name.clone(),
                (record.count, render_layout(&record.offset_layout)),
            ),
        })
        .collect()
}

fn dimension_list(manifest: &Manifest) -> String {
    if manifest.dimensions.is_empty() {
        return "none".to_owned();
    }
    manifest
        .dimensions
        .iter()
        .map(|dimension| format!("{}[{}]", dimension.name, dimension.count))
        .collect::<Vec<_>>()
        .join(", ")
}

fn field_changes(key: &str, old: &FieldView, new: &FieldView) -> Vec<String> {
    let mut lines = Vec::new();
    if old.address != new.address {
        lines.push(format!(
            "  ~ {key} address {} -> {}",
            render_address(&old.address),
            render_address(&new.address)
        ));
    }
    if render_codec(&old.codec) != render_codec(&new.codec) {
        lines.push(format!(
            "  ~ {key} codec {} -> {}",
            render_codec(&old.codec),
            render_codec(&new.codec)
        ));
    }
    if old.domain != new.domain {
        lines.push(format!(
            "  ~ {key} domain {} -> {}",
            render_domain(old.domain.as_ref()),
            render_domain(new.domain.as_ref())
        ));
    }
    if old.writable != new.writable {
        lines.push(format!(
            "  ~ {key} writable {} -> {}",
            old.writable, new.writable
        ));
    }
    if old.options != new.options {
        lines.push(format!(
            "  ~ {key} options {} -> {}",
            render_options(old.options.as_ref()),
            render_options(new.options.as_ref())
        ));
    }
    lines
}

fn menu_changes(old: &Menu, new: &Menu) -> Vec<String> {
    let (old_fields, new_fields) = (fields_of(old), fields_of(new));
    let mut lines = Vec::new();
    for (key, field) in &new_fields {
        if !old_fields.contains_key(key) {
            lines.push(format!(
                "  + {key} {} {}",
                render_address(&field.address),
                render_codec(&field.codec)
            ));
        }
    }
    for (key, field) in &old_fields {
        if !new_fields.contains_key(key) {
            lines.push(format!(
                "  - {key} {} {}",
                render_address(&field.address),
                render_codec(&field.codec)
            ));
        }
    }
    for (key, field) in &old_fields {
        if let Some(updated) = new_fields.get(key) {
            lines.extend(field_changes(key, field, updated));
        }
    }
    let (old_records, new_records) = (records_of(old), records_of(new));
    for name in new_records.keys() {
        if !old_records.contains_key(name) {
            lines.push(format!("  + record {name}"));
        }
    }
    for (name, (count, layout)) in &old_records {
        match new_records.get(name) {
            None => lines.push(format!("  - record {name}")),
            Some((new_count, new_layout)) => {
                if count != new_count {
                    lines.push(format!("  ~ record {name} count {count} -> {new_count}"));
                }
                if layout != new_layout {
                    lines.push(format!("  ~ record {name} layout {layout} -> {new_layout}"));
                }
            }
        }
    }
    lines
}

const fn plural(count: usize) -> &'static str {
    if count == 1 { "change" } else { "changes" }
}

fn release_line(label: &str, manifest: &Manifest) -> String {
    let digest: String = manifest
        .source
        .normalized_source_sha256
        .chars()
        .take(12)
        .collect();
    format!(
        "{label}: MCP {}, assembly {}, firmware {}, source {digest}",
        manifest.release.mcp_version,
        manifest.release.assembly_version,
        manifest.release.firmware_target
    )
}

/// Compare two manifests of the same radio.
///
/// # Errors
///
/// Returns an error when the manifests describe different radios.
pub fn diff_manifests(old: &Manifest, new: &Manifest) -> Result<DiffReport> {
    if old.model.radio != new.model.radio {
        return Err(extract_error!(
            "cannot diff different radios: {} versus {}",
            old.model.radio,
            new.model.radio
        ));
    }
    let mut lines = vec![
        "mcp-d75-extract diff".to_owned(),
        format!("radio: {}", old.model.radio),
        release_line("old", old),
        release_line("new", new),
        format!(
            "image_length: {} -> {}",
            old.model.image_length, new.model.image_length
        ),
    ];
    let mut differences = 0usize;
    if old.model.image_length != new.model.image_length {
        differences += 1;
    }
    let (old_dimensions, new_dimensions) = (dimension_list(old), dimension_list(new));
    if old.dimensions == new.dimensions {
        lines.push(format!("dimensions: unchanged ({old_dimensions})"));
    } else {
        lines.push(format!("dimensions: {old_dimensions} -> {new_dimensions}"));
        differences += 1;
    }
    let mut body = Vec::new();
    for old_menu in &old.menus {
        match new.menus.iter().find(|menu| menu.menu == old_menu.menu) {
            None => {
                body.push(String::new());
                body.push(format!("- menu {}", old_menu.menu));
                differences += 1;
            }
            Some(new_menu) => {
                let changes = menu_changes(old_menu, new_menu);
                if !changes.is_empty() {
                    body.push(String::new());
                    body.push(format!(
                        "menu {}: {} {}",
                        old_menu.menu,
                        changes.len(),
                        plural(changes.len())
                    ));
                    differences += changes.len();
                    body.extend(changes);
                }
            }
        }
    }
    for new_menu in &new.menus {
        if !old.menus.iter().any(|menu| menu.menu == new_menu.menu) {
            body.push(String::new());
            body.push(format!("+ menu {}", new_menu.menu));
            differences += 1;
        }
    }
    if body.is_empty() {
        body.push(String::new());
        body.push("no differences".to_owned());
    }
    lines.extend(body);
    lines.push(String::new());
    lines.push(format!(
        "summary: writable fields {} -> {}, enum options {} -> {}, combo mappings {} -> {}",
        old.summary.writable_registry_field_count,
        new.summary.writable_registry_field_count,
        old.summary.enum_option_count,
        new.summary.enum_option_count,
        old.summary.combo_option_mapping_count,
        new.summary.combo_option_mapping_count
    ));
    Ok(DiffReport { lines, differences })
}
