//! Full-manifest assembly from an `ILSpy` source tree.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::csharp::{
    Patterns, fancy_captures, find_balanced_body, normalize_whitespace, split_arguments,
};
use crate::error::{Result, extract_error};
use crate::language::{
    ComboMeta, parse_combo_options, parse_enum_definitions, parse_language_file,
};
use crate::records::extract_repeated_record_with;
use crate::sources::{
    Discovered, Sources, class_source_index, discover_serializers, parse_constants, parse_types,
    source_label,
};
use crate::tables::{
    GENERATOR, KNOWN_COMBO_ENUM_TYPE_COUNT, KNOWN_COMBO_OPTION_COUNT, KNOWN_OPERATION_COUNTS,
    REPEATED_RECORD_SPECS, SCHEMA_VERSION, SERIALIZERS, unsupported_radio_records,
};
use crate::value::{insert, obj_mut, req, req_array, req_i64, req_str, without_nulls};

/// Extract one top-level serializer into its manifest menu entry.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the reference extractor's signature"
)]
fn extract_menu(
    patterns: &Patterns,
    menu: &str,
    public_name: &str,
    class_name: &str,
    path: &Path,
    source: &str,
    source_dir: &Path,
    constants: &HashMap<String, i64>,
) -> Result<Value> {
    let types = parse_types(patterns, source)?;
    let (body, method_line) =
        find_balanced_body(source, r"^\s*public\s+void\s+a0\s*\(\s*m6\s+A_0\s*\)")?;
    let direct_mention_count = patterns.a0_mention_re.find_iter(&body).count();
    let direct_matches: Vec<(String, String)> = patterns
        .direct_call_re
        .captures_iter(&body)
        .map(|capture| {
            (
                capture
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                capture
                    .get(2)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    if direct_matches.len() != direct_mention_count {
        return Err(extract_error!(
            "{public_name}.a0 has {direct_mention_count} direct A_0 calls but only {} \
             match the supported one-line call shape",
            direct_matches.len()
        ));
    }
    let mut operations = Vec::new();
    for (sequence, (method, argument_text)) in direct_matches.iter().enumerate() {
        let args = split_arguments(argument_text);
        let (codec, name, role, offset) = crate::codecs::codec_for_call(
            patterns,
            method,
            &args,
            &types.properties,
            &types.private_fields,
            &types.enums,
            constants,
        )?;
        let mut operation = json!({
            "sequence": sequence,
            "role": role,
            "name": name,
            "offset": offset,
            "offset_hex": format!("0x{offset:04X}"),
            "codec": without_nulls(&codec)?,
        });
        if let Some(name) = name_of(&operation)
            && let Some(domain) = crate::tables::DIRECT_FIELD_DOMAINS.get(&format!("{menu}.{name}"))
        {
            insert(&mut operation, "domain", domain.clone())?;
        }
        operations.push(operation);
    }

    infer_blob_operations(&mut operations)?;

    let mut nested = Vec::new();
    for capture in fancy_captures(&patterns.nested_call_re, &body)? {
        let target = normalize_whitespace(capture.get(1).map(|m| m.as_str()).unwrap_or_default());
        let method = capture
            .get(2)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let index_expression = capture
            .get(3)
            .map(|m| m.as_str().trim().to_owned())
            .filter(|text| !text.is_empty());
        nested.push(json!({
            "target": target,
            "method": method,
            "index_expression": index_expression,
        }));
    }

    let field_count = operations
        .iter()
        .map(|operation| req_str(operation, "role").map(|role| u64::from(role == "field")))
        .sum::<Result<u64>>()?;
    Ok(json!({
        "menu": menu,
        "public_name": public_name,
        "csharp_class": class_name,
        "source_file": source_label(path, source_dir),
        "write_method": "a0(m6 A_0)",
        "write_method_line": method_line,
        "operation_count": operations.len(),
        "field_count": field_count,
        "operations": operations,
        "nested_serializers": nested,
    }))
}

/// Infer raw byte-array sizes from alternative clear-range operations at the
/// same offset (the bitmap branches use exactly this form) and classify the
/// two bitmap blobs.
fn infer_blob_operations(operations: &mut [Value]) -> Result<()> {
    let mut clear_lengths: HashMap<i64, Value> = HashMap::new();
    for operation in operations.iter() {
        if req_str(operation, "role")? == "clear" {
            drop(clear_lengths.insert(
                req_i64(operation, "offset")?,
                req(req(operation, "codec")?, "length")?.clone(),
            ));
        }
    }
    for operation in operations.iter_mut() {
        let offset = req_i64(operation, "offset")?;
        let codec_kind = req_str(req(operation, "codec")?, "kind")?.to_owned();
        if codec_kind == "raw_bytes"
            && let Some(length) = clear_lengths.get(&offset)
        {
            let codec = obj_mut(operation)?
                .get_mut("codec")
                .ok_or_else(|| extract_error!("operation lost its codec"))?;
            insert(codec, "length", length.clone())?;
        }
        let name = name_of(operation);
        if matches!(name.as_deref(), Some("PoweronBitmap" | "GpsLogBitmap")) {
            insert(operation, "category", Value::from("blob"))?;
        }
        if name.as_deref() == Some("GpsLogBitmap") {
            insert(operation, "writable", Value::from(false))?;
            insert(
                operation,
                "not_writable_reason",
                Value::from(
                    "the blob reaches protected factory-calibration pages; sparse radio writes \
                     must reject it before I/O",
                ),
            )?;
        }
    }
    Ok(())
}

fn name_of(operation: &Value) -> Option<String> {
    operation
        .get("name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// SHA-256 over label-tagged, newline-normalized selected sources.
pub(crate) fn normalized_source_digest(selected: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = selected.iter().collect();
    sorted.sort();
    let mut digest = Sha256::new();
    for (label, source) in sorted {
        digest.update(label.as_bytes());
        digest.update(b"\0");
        digest.update(source.replace("\r\n", "\n").as_bytes());
        digest.update(b"\0");
    }
    format!("{:x}", digest.finalize())
}

fn codec_matches_enum(codec: &Value, enum_name: &str) -> bool {
    codec.get("value_type").and_then(Value::as_str) == Some("enum")
        && codec.get("csharp_type").and_then(Value::as_str) == Some(enum_name)
}

fn tag_codec(container: &mut Value, qualified_name: &str) -> Result<()> {
    let codec = obj_mut(container)?
        .get_mut("codec")
        .ok_or_else(|| extract_error!("field lost its codec"))?;
    insert(codec, "enum_type", Value::from(qualified_name))
}

/// Collect `(owner class, enum type)` pairs used by a menu's enum-valued
/// codecs, parsing each contributing class's enum definitions along the way.
fn collect_used_enum_types(
    patterns: &Patterns,
    menu: &Value,
    class_name: &str,
    source: &str,
    class_sources: &HashMap<String, (PathBuf, String)>,
    definitions_by_class: &mut HashMap<String, HashMap<String, Value>>,
) -> Result<BTreeSet<(String, String)>> {
    drop(definitions_by_class.insert(
        class_name.to_owned(),
        parse_enum_definitions(patterns, source)?,
    ));
    let mut used_types: BTreeSet<(String, String)> = BTreeSet::new();
    for operation in req_array(menu, "operations")? {
        let codec = req(operation, "codec")?;
        if req_str(operation, "role")? == "field"
            && codec.get("value_type").and_then(Value::as_str) == Some("enum")
            && let Some(csharp_type) = codec.get("csharp_type").and_then(Value::as_str)
        {
            let _fresh = used_types.insert((class_name.to_owned(), csharp_type.to_owned()));
        }
    }
    for record in req_array(menu, "repeated_records")? {
        let record_class = record.get("source_class").and_then(Value::as_str);
        let (Some(record_class), true) = (record_class, record.get("expanded_fields").is_some())
        else {
            continue;
        };
        let (_path, record_source) = class_sources.get(record_class).ok_or_else(|| {
            extract_error!("source for repeated record class {record_class} not found")
        })?;
        drop(definitions_by_class.insert(
            record_class.to_owned(),
            parse_enum_definitions(patterns, record_source)?,
        ));
        for field in req_array(record, "fields")? {
            let codec = req(field, "codec")?;
            if req_str(field, "role")? == "field"
                && codec.get("value_type").and_then(Value::as_str) == Some("enum")
                && let Some(csharp_type) = codec.get("csharp_type").and_then(Value::as_str)
            {
                let _fresh = used_types.insert((record_class.to_owned(), csharp_type.to_owned()));
            }
        }
    }
    Ok(used_types)
}

/// Tag every codec in a menu that stores `owner_class.enum_name` with its
/// qualified enum type: top-level operations (only when the enum's owner is
/// the menu's own class), plus matching repeated-record fields and their
/// expanded copies.
fn tag_menu_enum_codecs(
    menu: &mut Value,
    tags_top_level: bool,
    owner_class: &str,
    enum_name: &str,
    qualified_name: &str,
) -> Result<()> {
    let operations = obj_mut(menu)?
        .get_mut("operations")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| extract_error!("menu lost its operations"))?;
    for operation in operations.iter_mut() {
        if req_str(operation, "role")? == "field"
            && tags_top_level
            && codec_matches_enum(req(operation, "codec")?, enum_name)
        {
            tag_codec(operation, qualified_name)?;
        }
    }
    let records = obj_mut(menu)?
        .get_mut("repeated_records")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| extract_error!("menu lost its repeated records"))?;
    for record in records.iter_mut() {
        if record.get("source_class").and_then(Value::as_str) != Some(owner_class) {
            continue;
        }
        if let Some(fields) = obj_mut(record)?
            .get_mut("fields")
            .and_then(Value::as_array_mut)
        {
            for field in fields.iter_mut() {
                if req_str(field, "role")? == "field"
                    && codec_matches_enum(req(field, "codec")?, enum_name)
                {
                    tag_codec(field, qualified_name)?;
                }
            }
        }
        if let Some(fields) = obj_mut(record)?
            .get_mut("expanded_fields")
            .and_then(Value::as_array_mut)
        {
            for field in fields.iter_mut() {
                if codec_matches_enum(req(field, "codec")?, enum_name) {
                    tag_codec(field, qualified_name)?;
                }
            }
        }
    }
    Ok(())
}

/// Attach per-menu enum catalogs and tag enum-valued codecs in place.
fn attach_enum_catalogs(
    patterns: &Patterns,
    menus: &mut [Value],
    discovered: &Discovered,
    class_sources: &HashMap<String, (PathBuf, String)>,
    combo_options: &HashMap<(String, String, String), ComboMeta>,
) -> Result<()> {
    for menu in menus.iter_mut() {
        let menu_name = req_str(menu, "menu")?.to_owned();
        let public_name = req_str(menu, "public_name")?.to_owned();
        let (class_name, _path, source) = discovered
            .iter()
            .find(|(name, _)| *name == menu_name)
            .map(|(_, entry)| entry)
            .ok_or_else(|| extract_error!("menu {menu_name} missing from discovery"))?;
        let mut definitions_by_class: HashMap<String, HashMap<String, Value>> = HashMap::new();
        let used_types = collect_used_enum_types(
            patterns,
            menu,
            class_name,
            source,
            class_sources,
            &mut definitions_by_class,
        )?;
        let mut catalogs = Vec::new();
        for (owner_class, enum_name) in &used_types {
            let definition = definitions_by_class
                .get(owner_class)
                .and_then(|definitions| definitions.get(enum_name))
                .ok_or_else(|| {
                    extract_error!(
                        "enum definition {owner_class}.{enum_name} used by \
                         {public_name} was not found"
                    )
                })?;
            let qualified_name = format!("{owner_class}.{enum_name}");
            let mut options = Vec::new();
            for raw_option in req_array(definition, "options")? {
                let mut option = raw_option.clone();
                let member = req_str(&option, "member")?.to_owned();
                if let Some(metadata) =
                    combo_options.get(&(owner_class.clone(), enum_name.clone(), member))
                {
                    if let Some(resource_key) = metadata.resource_key.as_deref() {
                        insert(&mut option, "resource_key", Value::from(resource_key))?;
                    }
                    if let Some(label) = metadata.label.as_deref() {
                        insert(&mut option, "label", Value::from(label))?;
                    }
                }
                options.push(option);
            }
            catalogs.push(json!({
                "name": qualified_name,
                "csharp_name": enum_name,
                "underlying_type": req(definition, "underlying_type")?.clone(),
                "options": options,
            }));
            let tags_top_level = owner_class == class_name;
            tag_menu_enum_codecs(
                menu,
                tags_top_level,
                owner_class,
                enum_name,
                &qualified_name,
            )?;
        }
        insert(menu, "enum_types", Value::Array(catalogs))?;
    }
    Ok(())
}

fn python_dict_repr(pairs: &[(&str, u64)]) -> String {
    let body = pairs
        .iter()
        .map(|(key, value)| format!("'{key}': {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

/// Options controlling schema extraction.
#[derive(Debug, Default, Clone)]
pub struct BuildOptions {
    /// Require the reviewed 134/17/85/31 direct-operation counts.
    pub strict_known_layout: bool,
    /// Optional UTF-16 MCP-D75 `Language/English.lng` for option labels.
    pub language_file: Option<PathBuf>,
}

/// Extract the full manifest schema from an `ILSpy` source tree.
///
/// # Errors
///
/// Returns an error when the source tree cannot be read or decoded, when any
/// serializer deviates from the reviewed one-line call shape, or when
/// `strict_known_layout` detects changed operation or combo-mapping counts.
pub fn build_schema(source_dir: &Path, options: &BuildOptions) -> Result<Value> {
    let patterns = Patterns::new()?;
    let sources: Sources = crate::sources::read_sources(source_dir)?;
    let discovered = discover_serializers(&patterns, &sources)?;
    let class_sources = class_source_index(&patterns, &sources)?;
    let constants = parse_constants(&patterns, &sources);
    let (language_sections, language_provenance) =
        parse_language_file(options.language_file.as_ref())?;
    let record_classes: BTreeSet<&str> = REPEATED_RECORD_SPECS
        .iter()
        .flat_map(|(_, specs)| specs.iter())
        .map(|spec| spec.source_class)
        .filter(|source_class| class_sources.contains_key(*source_class))
        .collect();
    let serializer_classes: Vec<String> = discovered
        .iter()
        .map(|(_, (class_name, _, _))| class_name.clone())
        .chain(record_classes.iter().map(|name| (*name).to_owned()))
        .collect();
    let combo_options =
        parse_combo_options(&patterns, &sources, &language_sections, &serializer_classes)?;
    let mut menus = Vec::new();
    for (menu, (class_name, path, source)) in &discovered {
        let public_name = SERIALIZERS
            .iter()
            .find(|(name, _)| name == menu)
            .map(|(_, public)| *public)
            .ok_or_else(|| extract_error!("unknown menu {menu}"))?;
        menus.push(extract_menu(
            &patterns,
            menu,
            public_name,
            class_name,
            path,
            source,
            source_dir,
            &constants,
        )?);
    }
    for menu in &mut menus {
        attach_repeated_records(&patterns, menu, &class_sources, source_dir, &constants)?;
    }
    attach_enum_catalogs(
        &patterns,
        &mut menus,
        &discovered,
        &class_sources,
        &combo_options,
    )?;
    if options.strict_known_layout {
        check_known_layout(&menus, &combo_options)?;
    }

    let selected_sources: Vec<(String, String)> = sources
        .iter()
        .map(|(path, source)| (source_label(path, source_dir), source.clone()))
        .collect();
    let mut serializer_class_map = Map::new();
    for (menu, _public_name) in SERIALIZERS {
        let class_name = discovered
            .iter()
            .find(|(name, _)| *name == menu)
            .map(|(_, (class_name, _, _))| class_name.clone())
            .ok_or_else(|| extract_error!("menu {menu} missing from discovery"))?;
        drop(serializer_class_map.insert(menu.to_owned(), Value::from(class_name)));
    }
    let mut source_metadata = json!({
        "kind": "ILSpy C# project",
        "normalized_source_sha256": normalized_source_digest(&selected_sources),
        "serializer_classes": serializer_class_map,
    });
    if let Some(provenance) = language_provenance {
        insert(&mut source_metadata, "language_file", provenance)?;
    }
    let summary = summary_value(&menus, &combo_options)?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "generator": GENERATOR,
        "source": source_metadata,
        "summary": summary,
        "menus": menus,
    }))
}

/// Extract and attach a menu's repeated records: the specs' public list
/// serializers, plus the cataloged non-public radio records.
fn attach_repeated_records(
    patterns: &Patterns,
    menu: &mut Value,
    class_sources: &HashMap<String, (PathBuf, String)>,
    source_dir: &Path,
    constants: &HashMap<String, i64>,
) -> Result<()> {
    let menu_name = req_str(menu, "menu")?.to_owned();
    let nested_count = req_array(menu, "nested_serializers")?.len();
    let mut repeated_records = Vec::new();
    if menu_name == "radio" && nested_count == 3 {
        repeated_records.extend(unsupported_radio_records());
    }
    if nested_count > 0 {
        let specs = REPEATED_RECORD_SPECS
            .iter()
            .find(|(name, _)| *name == menu_name)
            .map(|(_, specs)| *specs)
            .unwrap_or_default();
        for spec in specs {
            let (record_path, record_source) =
                class_sources.get(spec.source_class).ok_or_else(|| {
                    extract_error!(
                        "nested serializer source {} for {menu_name}.{} was not found",
                        spec.source_class,
                        spec.name
                    )
                })?;
            repeated_records.push(extract_repeated_record_with(
                patterns,
                spec,
                record_path,
                record_source,
                source_dir,
                constants,
            )?);
        }
    }
    insert(menu, "repeated_records", Value::Array(repeated_records))
}

/// Enforce the reviewed operation and combo-mapping counts.
fn check_known_layout(
    menus: &[Value],
    combo_options: &HashMap<(String, String, String), ComboMeta>,
) -> Result<()> {
    let mut actual = Vec::new();
    for menu in menus {
        actual.push((
            req_str(menu, "menu")?.to_owned(),
            u64::try_from(req_i64(menu, "operation_count")?).unwrap_or_default(),
        ));
    }
    let expected: HashMap<&str, u64> = KNOWN_OPERATION_COUNTS.iter().copied().collect();
    let actual_map: HashMap<&str, u64> = actual
        .iter()
        .map(|(name, count)| (name.as_str(), *count))
        .collect();
    if expected != actual_map {
        let actual_pairs: Vec<(&str, u64)> = actual
            .iter()
            .map(|(name, count)| (name.as_str(), *count))
            .collect();
        return Err(extract_error!(
            "known-layout operation counts changed: expected {}, got {}",
            python_dict_repr(&KNOWN_OPERATION_COUNTS),
            python_dict_repr(&actual_pairs)
        ));
    }
    let combo_enum_types: HashSet<(&String, &String)> = combo_options
        .keys()
        .map(|(owner, enum_name, _)| (owner, enum_name))
        .collect();
    if combo_enum_types.len() != KNOWN_COMBO_ENUM_TYPE_COUNT
        || combo_options.len() != KNOWN_COMBO_OPTION_COUNT
    {
        return Err(extract_error!(
            "known-layout combo mappings changed: expected {KNOWN_COMBO_ENUM_TYPE_COUNT} \
             enum types/{KNOWN_COMBO_OPTION_COUNT} options, got {} enum types/{} options",
            combo_enum_types.len(),
            combo_options.len()
        ));
    }
    Ok(())
}

/// Compute the manifest's summary object from the fully assembled menus.
fn summary_value(
    menus: &[Value],
    combo_options: &HashMap<(String, String, String), ComboMeta>,
) -> Result<Value> {
    let mut operation_count: u64 = 0;
    let mut field_count: u64 = 0;
    let mut constant_operation_count: u64 = 0;
    let mut internal_operation_count: u64 = 0;
    let mut clear_operation_count: u64 = 0;
    let mut nested_serializer_call_count: u64 = 0;
    let mut repeated_record_type_count: u64 = 0;
    let mut unsupported_public_record_type_count: u64 = 0;
    let mut writable_top_level_fields: u64 = 0;
    let mut repeated_fields: Vec<&Value> = Vec::new();
    let mut enum_type_count: u64 = 0;
    let mut enum_options: Vec<&Value> = Vec::new();
    for menu in menus {
        operation_count += u64::try_from(req_i64(menu, "operation_count")?).unwrap_or_default();
        field_count += u64::try_from(req_i64(menu, "field_count")?).unwrap_or_default();
        for operation in req_array(menu, "operations")? {
            let role = req_str(operation, "role")?;
            constant_operation_count += u64::from(role == "constant");
            internal_operation_count += u64::from(role == "internal");
            clear_operation_count += u64::from(role == "clear");
            let writable = operation
                .get("writable")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            writable_top_level_fields += u64::from(role == "field" && writable);
        }
        nested_serializer_call_count +=
            u64::try_from(req_array(menu, "nested_serializers")?.len()).unwrap_or_default();
        for record in req_array(menu, "repeated_records")? {
            repeated_record_type_count += u64::from(record.get("expanded_fields").is_some());
            unsupported_public_record_type_count +=
                u64::from(record.get("unsupported_public_reason").is_some());
            if let Some(expanded) = record.get("expanded_fields").and_then(Value::as_array) {
                repeated_fields.extend(expanded.iter());
            }
        }
        for catalog in req_array(menu, "enum_types")? {
            enum_type_count += 1;
            enum_options.extend(req_array(catalog, "options")?.iter());
        }
    }
    let writable_repeated_fields: u64 = repeated_fields
        .iter()
        .map(|field| {
            u64::from(
                field
                    .get("writable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            )
        })
        .sum();
    let labeled_enum_option_count: u64 = enum_options
        .iter()
        .map(|option| u64::from(option.get("label").is_some()))
        .sum();
    let resource_enum_option_count: u64 = enum_options
        .iter()
        .map(|option| u64::from(option.get("resource_key").is_some()))
        .sum();
    let combo_enum_types: HashSet<(&String, &String)> = combo_options
        .keys()
        .map(|(owner, enum_name, _)| (owner, enum_name))
        .collect();
    let repeated_field_count = u64::try_from(repeated_fields.len()).unwrap_or_default();
    Ok(json!({
        "menu_count": menus.len(),
        "operation_count": operation_count,
        "field_count": field_count,
        "expanded_record_field_count": repeated_field_count,
        "total_public_field_count": field_count + repeated_field_count,
        "writable_registry_field_count": writable_top_level_fields + writable_repeated_fields,
        "constant_operation_count": constant_operation_count,
        "internal_operation_count": internal_operation_count,
        "clear_operation_count": clear_operation_count,
        "nested_serializer_call_count": nested_serializer_call_count,
        "repeated_record_type_count": repeated_record_type_count,
        "unsupported_public_record_type_count": unsupported_public_record_type_count,
        "enum_type_count": enum_type_count,
        "enum_option_count": enum_options.len(),
        "labeled_enum_option_count": labeled_enum_option_count,
        "resource_enum_option_count": resource_enum_option_count,
        "combo_enum_type_count": combo_enum_types.len(),
        "combo_option_mapping_count": combo_options.len(),
    }))
}

/// Serialize the schema as stable, indented JSON text.
///
/// # Errors
///
/// Returns an error when the schema value cannot be serialized.
pub fn json_text(schema: &Value) -> Result<String> {
    let rendered = serde_json::to_string_pretty(schema)
        .map_err(|error| extract_error!("cannot serialize schema: {error}"))?;
    Ok(format!("{rendered}\n"))
}
