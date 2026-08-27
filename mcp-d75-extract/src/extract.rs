//! Manifest assembly: discovery, per-menu extraction, catalogs, checks, summary.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::address::SlotSymbol;
use crate::class_index::{ClassIndex, ClassInfo};
use crate::csharp::Patterns;
use crate::discovery::{
    Discovered, MenuInfo, NestedCall, discover, field_type, list_element, nested_calls,
    resolve_list_target, slot_symbols,
};
use crate::enums::attach_enum_catalogs;
use crate::error::{Result, extract_error};
use crate::language::{parse_combo_options, parse_language_file};
use crate::manifest::{
    Anchor, Dimension, LanguageFileInfo, Manifest, Menu, ModelInfo, NestedSerializer, Operation,
    PrivateRecord, Record, RecordEntry, ReleaseInfo, Role, SCHEMA_VERSION, SourceInfo, Summary,
    WriteMethodRef,
};
use crate::model::{ModelSpec, RecordSpec};
use crate::operations::{WriteScope, extract_operations};
use crate::records::{Owner, catalog_private, extract_record};
use crate::sources::{Sources, parse_constants, parse_types, read_sources, source_label};

/// Options controlling one extraction run.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Radio spec.
    pub model: &'static ModelSpec,
    /// Declared MCP marketing version.
    pub mcp_version: String,
    /// Declared firmware target.
    pub firmware_target: String,
    /// Optional UTF-16 language file for option labels.
    pub language_file: Option<PathBuf>,
    /// Require the spec's reviewed counts.
    pub strict_known_layout: bool,
}

/// Assembly version from `Properties/AssemblyInfo.cs`, after checking the product.
fn assembly_info(patterns: &Patterns, sources: &Sources, spec: &ModelSpec) -> Result<String> {
    let mut products = BTreeSet::new();
    let mut versions = BTreeSet::new();
    for (_, source) in sources {
        for capture in patterns.assembly_product_re.captures_iter(source) {
            let _fresh = products.insert(
                capture
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
        for capture in patterns.assembly_version_re.captures_iter(source) {
            let _fresh = versions.insert(
                capture
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
    }
    let product = match products.len() {
        1 => products.into_iter().next().unwrap_or_default(),
        0 => {
            return Err(extract_error!(
                "no [assembly: AssemblyProduct] attribute found; the project must include Properties/AssemblyInfo.cs"
            ));
        }
        _ => {
            return Err(extract_error!(
                "several AssemblyProduct attributes found: {products:?}"
            ));
        }
    };
    if product != spec.mcp_product {
        return Err(extract_error!(
            "assembly product {product:?} is not the {} program {:?}",
            spec.product,
            spec.mcp_product
        ));
    }
    match versions.len() {
        1 => Ok(versions.into_iter().next().unwrap_or_default()),
        0 => Err(extract_error!(
            "no [assembly: AssemblyVersion] attribute found"
        )),
        _ => Err(extract_error!(
            "several AssemblyVersion attributes found: {versions:?}"
        )),
    }
}

/// SHA-256 over label-tagged, newline-normalized sources (unchanged from v3).
fn normalized_source_digest(sources: &Sources, source_dir: &Path) -> String {
    let mut selected: Vec<(String, &String)> = sources
        .iter()
        .map(|(path, source)| (source_label(path, source_dir), source))
        .collect();
    selected.sort();
    let mut digest = Sha256::new();
    for (label, source) in selected {
        digest.update(label.as_bytes());
        digest.update(b"\0");
        digest.update(source.replace("\r\n", "\n").as_bytes());
        digest.update(b"\0");
    }
    format!("{:x}", digest.finalize())
}

/// A nested call together with the class and slots it was made from.
struct OwnedCall<'a> {
    owner: &'a ClassInfo,
    owner_slots: &'a [SlotSymbol],
    call: NestedCall,
}

/// Shared inputs of one menu's extraction.
struct MenuBuild<'a> {
    patterns: &'a Patterns,
    spec: &'static ModelSpec,
    index: &'a ClassIndex,
    constants: &'a HashMap<String, i64>,
    writer_class: &'a str,
    strict: bool,
}

impl MenuBuild<'_> {
    const fn scope<'s>(&'s self, slots: &'s [SlotSymbol]) -> WriteScope<'s> {
        WriteScope {
            patterns: self.patterns,
            spec: self.spec,
            index: self.index,
            constants: self.constants,
            slots,
            overrides: &[],
        }
    }

    const fn owner<'s>(&'s self, class: &'s ClassInfo, slots: &'s [SlotSymbol]) -> Owner<'s> {
        Owner {
            index: self.index,
            class,
            slots,
            writer_class: self.writer_class,
        }
    }
}

/// True for the serializer's loop over its per-slot detail list.
fn is_detail_call(
    patterns: &Patterns,
    menu: &MenuInfo,
    owner: &ClassInfo,
    call: &NestedCall,
) -> bool {
    let Some(detail) = &menu.detail else {
        return false;
    };
    let (field, index) = call.split_target();
    owner.name == menu.class
        && index.is_some()
        && call.index_expression.is_none()
        && field == detail.list_field
        && field_type(patterns, owner, &field)
            .as_deref()
            .and_then(list_element)
            == Some(detail.class.as_str())
}

/// Nested calls into one private class, grouped in first-call order.
struct PrivateGroup<'a> {
    owner: &'a ClassInfo,
    owner_slots: &'a [SlotSymbol],
    target: &'a ClassInfo,
    calls: Vec<&'a NestedCall>,
}

/// The menu's nested calls split into record lists and private groups.
struct MenuCalls<'a> {
    extracted: Vec<(&'static RecordSpec, Record)>,
    groups: Vec<PrivateGroup<'a>>,
}

/// Split the menu's nested calls into extracted record lists and private groups.
fn classify_calls<'a>(
    build: &MenuBuild<'a>,
    menu: &MenuInfo,
    calls: &'a [OwnedCall<'a>],
) -> Result<MenuCalls<'a>> {
    let mut extracted: Vec<(&'static RecordSpec, Record)> = Vec::new();
    let mut groups: Vec<PrivateGroup<'a>> = Vec::new();
    for owned in calls {
        if let Ok(list) = resolve_list_target(build.patterns, owned.owner, &owned.call.target) {
            let spec = build
                .spec
                .records_for(menu.key)
                .into_iter()
                .find(|spec| spec.list == list.property)
                .ok_or_else(|| {
                    extract_error!(
                        "{}: public record list {} is not in the {} spec",
                        owned.owner.name,
                        list.property,
                        build.spec.id
                    )
                })?;
            if extracted.iter().any(|(seen, _)| seen.list == spec.list) {
                return Err(extract_error!(
                    "record list {} is called more than once",
                    spec.list
                ));
            }
            let scope = build.scope(owned.owner_slots);
            let owner = build.owner(owned.owner, owned.owner_slots);
            extracted.push((spec, extract_record(&scope, &owner, spec, &owned.call)?));
            continue;
        }
        let (field, _) = owned.call.split_target();
        let declared = field_type(build.patterns, owned.owner, &field).ok_or_else(|| {
            extract_error!(
                "{}: nested call target {} is not a declared field",
                owned.owner.name,
                owned.call.target
            )
        })?;
        let element = list_element(&declared).unwrap_or(&declared).to_owned();
        let target = build.index.resolve(owned.owner, &element).ok_or_else(|| {
            extract_error!(
                "{}: class {element} of nested call target {} not found",
                owned.owner.name,
                owned.call.target
            )
        })?;
        let mut public: Vec<String> = parse_types(build.patterns, &target.name, &target.own_text)?
            .properties
            .into_keys()
            .filter(|property| !build.spec.is_anchor(property))
            .collect();
        if !public.is_empty() {
            public.sort();
            return Err(extract_error!(
                "{}: nested call {}.{} targets a class with public properties ({}) that is not a spec'd record list",
                owned.owner.name,
                owned.call.target,
                owned.call.method,
                public.join(", ")
            ));
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.target.name == target.name)
        {
            group.calls.push(&owned.call);
        } else {
            groups.push(PrivateGroup {
                owner: owned.owner,
                owner_slots: owned.owner_slots,
                target,
                calls: vec![&owned.call],
            });
        }
    }
    Ok(MenuCalls { extracted, groups })
}

/// Match private groups to the spec's catalog entries by verification.
fn match_private_groups(
    build: &MenuBuild<'_>,
    menu: &MenuInfo,
    groups: Vec<PrivateGroup<'_>>,
) -> Result<Vec<PrivateRecord>> {
    let mut used: Vec<&str> = Vec::new();
    let mut privates = Vec::new();
    for group in groups {
        let scope = build.scope(group.owner_slots);
        let owner = build.owner(group.owner, group.owner_slots);
        let mut matched = None;
        let mut failures = Vec::new();
        for spec in build.spec.private_writers_for(menu.key) {
            if used.contains(&spec.name) {
                continue;
            }
            match catalog_private(&scope, &owner, spec, group.target, &group.calls) {
                Ok(entry) => {
                    matched = Some((spec.name, entry));
                    break;
                }
                Err(error) => failures.push(format!("{}: {error}", spec.name)),
            }
        }
        let (name, entry) = matched.ok_or_else(|| {
            extract_error!(
                "{}: private sub-writer {} matches no spec entry: {}",
                group.owner.name,
                group.target.name,
                failures.join("; ")
            )
        })?;
        used.push(name);
        privates.push(entry);
    }
    Ok(privates)
}

/// Record entries in manifest order: spec'd lists, then spec'd private writers.
fn collect_records(
    build: &MenuBuild<'_>,
    menu: &MenuInfo,
    calls: &[OwnedCall<'_>],
) -> Result<Vec<RecordEntry>> {
    let MenuCalls { extracted, groups } = classify_calls(build, menu, calls)?;
    let privates = match_private_groups(build, menu, groups)?;
    let mut entries: Vec<RecordEntry> = Vec::new();
    for spec in build.spec.records_for(menu.key) {
        match extracted
            .iter()
            .find(|(candidate, _)| candidate.list == spec.list)
        {
            Some((_, record)) => entries.push(RecordEntry::Extracted(record.clone())),
            None if build.strict => {
                return Err(extract_error!(
                    "record list {} was not found in menu {}",
                    spec.list,
                    menu.key
                ));
            }
            None => {}
        }
    }
    for spec in build.spec.private_writers_for(menu.key) {
        match privates.iter().find(|entry| entry.name == spec.name) {
            Some(entry) => entries.push(RecordEntry::Unsupported(entry.clone())),
            None if build.strict => {
                return Err(extract_error!(
                    "private writer {} was not found in menu {}",
                    spec.name,
                    menu.key
                ));
            }
            None => {}
        }
    }
    Ok(entries)
}

fn nested_serializer(call: &NestedCall) -> NestedSerializer {
    NestedSerializer {
        target: call.target.clone(),
        method: call.method.clone(),
        index_expression: call.index_expression.clone(),
    }
}

fn build_menu(build: &MenuBuild<'_>, menu: &MenuInfo) -> Result<Menu> {
    let serializer = build
        .index
        .get(&menu.class)
        .ok_or_else(|| extract_error!("serializer {} vanished", menu.class))?;
    let serializer_slots = slot_symbols(build.patterns, serializer, build.spec)?;
    let scope = build.scope(&serializer_slots);
    let mut operations: Vec<Operation> =
        extract_operations(&scope, serializer, &menu.write, menu.key, 0)?;
    let serializer_calls = nested_calls(build.patterns, &menu.write.body)?;
    let mut nested: Vec<NestedSerializer> =
        serializer_calls.iter().map(nested_serializer).collect();
    let mut calls: Vec<OwnedCall<'_>> = serializer_calls
        .into_iter()
        .filter(|call| !is_detail_call(build.patterns, menu, serializer, call))
        .map(|call| OwnedCall {
            owner: serializer,
            owner_slots: &serializer_slots,
            call,
        })
        .collect();
    if let Some(detail) = &menu.detail {
        let detail_class = build
            .index
            .get(&detail.class)
            .ok_or_else(|| extract_error!("detail {} vanished", detail.class))?;
        let detail_scope = build.scope(&detail.slots);
        let first = u64::try_from(operations.len())
            .map_err(|_| extract_error!("operation count overflow"))?;
        operations.extend(extract_operations(
            &detail_scope,
            detail_class,
            &detail.write,
            menu.key,
            first,
        )?);
        for call in nested_calls(build.patterns, &detail.write.body)? {
            nested.push(nested_serializer(&call));
            calls.push(OwnedCall {
                owner: detail_class,
                owner_slots: &detail.slots,
                call,
            });
        }
    }
    let repeated_records = collect_records(build, menu, &calls)?;
    let field_count = operations
        .iter()
        .filter(|operation| operation.role == Role::Field)
        .count();
    Ok(Menu {
        menu: menu.key.to_owned(),
        public_name: menu.property.to_owned(),
        csharp_class: menu.class.clone(),
        source_file: serializer.label.clone(),
        write_method: menu.write.signature.clone(),
        write_method_line: u64::try_from(menu.write.line)
            .map_err(|_| extract_error!("line overflow"))?,
        detail_class: menu.detail.as_ref().map(|detail| detail.class.clone()),
        detail_write_method: menu
            .detail
            .as_ref()
            .map(|detail| detail.write.signature.clone()),
        detail_write_method_line: menu
            .detail
            .as_ref()
            .map(|detail| u64::try_from(detail.write.line).unwrap_or_default()),
        operation_count: u64::try_from(operations.len())
            .map_err(|_| extract_error!("operation count overflow"))?,
        field_count: u64::try_from(field_count)
            .map_err(|_| extract_error!("field count overflow"))?,
        operations,
        nested_serializers: nested,
        repeated_records,
        enum_types: Vec::new(),
    })
}

fn check_known_layout(
    spec: &ModelSpec,
    menus: &[Menu],
    combo_enum_types: usize,
    combo_options: usize,
) -> Result<()> {
    let actual: Vec<(&str, u64)> = menus
        .iter()
        .map(|menu| (menu.menu.as_str(), menu.operation_count))
        .collect();
    if actual != spec.reviewed.operation_counts {
        return Err(extract_error!(
            "known-layout operation counts changed: expected {:?}, got {actual:?}",
            spec.reviewed.operation_counts
        ));
    }
    if combo_enum_types != spec.reviewed.combo_enum_types
        || combo_options != spec.reviewed.combo_options
    {
        return Err(extract_error!(
            "known-layout combo mappings changed: expected {} enum types/{} options, got {combo_enum_types} enum types/{combo_options} options",
            spec.reviewed.combo_enum_types,
            spec.reviewed.combo_options
        ));
    }
    Ok(())
}

fn count(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| extract_error!("summary count overflow"))
}

fn summarize_menu(summary: &mut Summary, menu: &Menu) -> Result<()> {
    summary.operation_count += menu.operation_count;
    summary.field_count += menu.field_count;
    for operation in &menu.operations {
        match operation.role {
            Role::Constant => summary.constant_operation_count += 1,
            Role::Internal => summary.internal_operation_count += 1,
            Role::Clear => summary.clear_operation_count += 1,
            Role::Field => {
                if operation.writable.unwrap_or(true) {
                    summary.writable_registry_field_count += 1;
                    if !operation.address.is_absolute() {
                        summary.slot_relative_field_count += 1;
                    }
                }
            }
        }
    }
    summary.nested_serializer_call_count += count(menu.nested_serializers.len())?;
    for entry in &menu.repeated_records {
        match entry {
            RecordEntry::Extracted(record) => {
                summary.repeated_record_type_count += 1;
                summary.expanded_record_field_count += count(record.expanded_fields.len())?;
                for field in &record.expanded_fields {
                    if field.writable.unwrap_or(true) {
                        summary.writable_registry_field_count += 1;
                        if !field.address.is_absolute() {
                            summary.slot_relative_field_count += 1;
                        }
                    }
                }
            }
            RecordEntry::Unsupported(_) => summary.unsupported_public_record_type_count += 1,
        }
    }
    for catalog in &menu.enum_types {
        summary.enum_type_count += 1;
        summary.enum_option_count += count(catalog.options.len())?;
        summary.labeled_enum_option_count += count(
            catalog
                .options
                .iter()
                .filter(|option| option.label.is_some())
                .count(),
        )?;
        summary.resource_enum_option_count += count(
            catalog
                .options
                .iter()
                .filter(|option| option.resource_key.is_some())
                .count(),
        )?;
    }
    Ok(())
}

fn summarize(
    menus: &[Menu],
    dimensions: usize,
    combo_enum_types: usize,
    combo_options: usize,
) -> Result<Summary> {
    let mut summary = Summary {
        menu_count: count(menus.len())?,
        operation_count: 0,
        field_count: 0,
        expanded_record_field_count: 0,
        total_public_field_count: 0,
        writable_registry_field_count: 0,
        constant_operation_count: 0,
        internal_operation_count: 0,
        clear_operation_count: 0,
        nested_serializer_call_count: 0,
        repeated_record_type_count: 0,
        unsupported_public_record_type_count: 0,
        enum_type_count: 0,
        enum_option_count: 0,
        labeled_enum_option_count: 0,
        resource_enum_option_count: 0,
        combo_enum_type_count: count(combo_enum_types)?,
        combo_option_mapping_count: count(combo_options)?,
        dimension_count: count(dimensions)?,
        slot_relative_field_count: 0,
    };
    for menu in menus {
        summarize_menu(&mut summary, menu)?;
    }
    summary.total_public_field_count = summary.field_count + summary.expanded_record_field_count;
    Ok(summary)
}

fn source_info(
    discovered: &Discovered,
    digest: String,
    language: Option<LanguageFileInfo>,
) -> Result<SourceInfo> {
    let mut write_methods = Vec::new();
    let mut detail_classes = Vec::new();
    for menu in &discovered.menus {
        write_methods.push((
            menu.key.to_owned(),
            WriteMethodRef {
                class: menu.write.class.clone(),
                method: menu.write.method.clone(),
                line: u64::try_from(menu.write.line)
                    .map_err(|_| extract_error!("line overflow"))?,
            },
        ));
        if let Some(detail) = &menu.detail {
            detail_classes.push((menu.key.to_owned(), detail.class.clone()));
        }
    }
    Ok(SourceInfo {
        kind: "ILSpy C# project".to_owned(),
        normalized_source_sha256: digest,
        serializer_classes: discovered
            .menus
            .iter()
            .map(|menu| (menu.key.to_owned(), menu.class.clone()))
            .collect(),
        writer_class: discovered.writer_class.clone(),
        resource_class: discovered.resource_class.clone(),
        write_methods,
        detail_classes,
        language_file: language,
    })
}

/// Extract the full manifest from an `ILSpy` project directory.
///
/// # Errors
///
/// Returns an error when the sources cannot be read or decoded, when
/// discovery finds zero or several candidates for any anchor, when a writer
/// deviates from the one-line call shape, when a pinned fact fails
/// verification, or when `strict_known_layout` detects changed counts.
pub fn build_manifest(source_dir: &Path, options: &BuildOptions) -> Result<Manifest> {
    let spec = options.model;
    let patterns = Patterns::new()?;
    let sources = read_sources(source_dir)?;
    let index = ClassIndex::build(&patterns, &sources, source_dir)?;
    let constants = parse_constants(&patterns, &sources);
    let assembly_version = assembly_info(&patterns, &sources, spec)?;
    let discovered = discover(&index, &sources, spec, &patterns)?;
    let (sections, language) = parse_language_file(options.language_file.as_ref())?;
    let build = MenuBuild {
        patterns: &patterns,
        spec,
        index: &index,
        constants: &constants,
        writer_class: &discovered.writer_class,
        strict: options.strict_known_layout,
    };
    let mut menus = Vec::new();
    for menu in &discovered.menus {
        menus.push(build_menu(&build, menu)?);
    }
    let mut classes: Vec<String> = discovered
        .menus
        .iter()
        .map(|menu| menu.class.clone())
        .collect();
    classes.extend(
        discovered
            .menus
            .iter()
            .filter_map(|menu| menu.detail.as_ref().map(|detail| detail.class.clone())),
    );
    for menu in &menus {
        for entry in &menu.repeated_records {
            if let RecordEntry::Extracted(record) = entry {
                classes.push(record.source_class.clone());
            }
        }
    }
    let combos = parse_combo_options(
        &patterns,
        &sources,
        &sections,
        &classes,
        &discovered.resource_class,
    )?;
    attach_enum_catalogs(&patterns, &index, &mut menus, &combos)?;
    let combo_enum_types = combos
        .keys()
        .map(|(owner, name, _)| (owner.clone(), name.clone()))
        .collect::<BTreeSet<_>>()
        .len();
    if options.strict_known_layout {
        check_known_layout(spec, &menus, combo_enum_types, combos.len())?;
    }
    let dimensions: Vec<Dimension> = spec
        .dimensions
        .iter()
        .map(|dimension| Dimension {
            name: dimension.name.to_owned(),
            count: dimension.count,
            anchors: dimension
                .anchors
                .iter()
                .map(|anchor| Anchor {
                    property: anchor.property.to_owned(),
                    stride: anchor.stride,
                })
                .collect(),
        })
        .collect();
    let summary = summarize(&menus, dimensions.len(), combo_enum_types, combos.len())?;
    Ok(Manifest {
        schema_version: SCHEMA_VERSION,
        generator: env!("CARGO_PKG_NAME").to_owned(),
        model: ModelInfo {
            radio: spec.id.to_owned(),
            product: spec.product.to_owned(),
            mcp_product: spec.mcp_product.to_owned(),
            image_length: spec.image_length,
        },
        release: ReleaseInfo {
            mcp_version: options.mcp_version.clone(),
            assembly_version,
            firmware_target: options.firmware_target.clone(),
        },
        dimensions,
        source: source_info(
            &discovered,
            normalized_source_digest(&sources, source_dir),
            language,
        )?,
        summary,
        menus,
    })
}
