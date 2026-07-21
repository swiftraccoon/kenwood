//! Extraction of the public, statically sized repeated-record serializers.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::codecs::codec_for_call;
use crate::csharp::{Patterns, compact_expression, find_balanced_body, split_arguments};
use crate::error::{Result, extract_error};
use crate::sources::{parse_types, source_label};
use crate::tables::{RECORD_SYMBOLS, RecordSpec, fixed_string_padding_override};
use crate::value::{display_name, insert, req, req_i64, req_str, without_nulls};

/// Extract the child writer's checked linear or one-override base formula.
fn record_offset_layout(
    patterns: &Patterns,
    body: &str,
    count: usize,
) -> Result<(String, Value, Vec<i64>)> {
    let assignments: Vec<(String, String)> = patterns
        .base_assign_re
        .captures_iter(body)
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
    if assignments.len() != 1 {
        return Err(extract_error!(
            "record writer must contain exactly one A_1 base assignment, got {assignments:?}"
        ));
    }
    let (variable, expression) = assignments
        .into_iter()
        .next()
        .ok_or_else(|| extract_error!("record writer base assignment vanished"))?;
    let compact = compact_expression(&expression);
    if let Some(linear) = patterns.linear_base_re.captures(&compact) {
        let base: i64 = linear
            .get(1)
            .and_then(|digits| digits.as_str().parse().ok())
            .ok_or_else(|| extract_error!("unparsable record base in {compact}"))?;
        let stride: i64 = linear
            .get(2)
            .and_then(|digits| digits.as_str().parse().ok())
            .ok_or_else(|| extract_error!("unparsable record stride in {compact}"))?;
        let count_i64 = i64::try_from(count)
            .map_err(|_| extract_error!("record count {count} exceeds supported range"))?;
        let bases = (0..count_i64).map(|index| base + stride * index).collect();
        return Ok((
            variable,
            json!({"kind": "linear", "base": base, "stride": stride}),
            bases,
        ));
    }
    if let Some(piecewise) = patterns.piecewise_base_re.captures(&compact) {
        let field = |index: usize| -> Result<i64> {
            piecewise
                .get(index)
                .and_then(|digits| digits.as_str().parse().ok())
                .ok_or_else(|| extract_error!("unparsable piecewise record base in {compact}"))
        };
        let override_index = field(1)?;
        let override_base = field(2)?;
        let base = field(3)?;
        let stride = field(4)?;
        let count_i64 = i64::try_from(count)
            .map_err(|_| extract_error!("record count {count} exceeds supported range"))?;
        if !(0..count_i64).contains(&override_index) {
            return Err(extract_error!(
                "record base override index {override_index} is outside count {count}"
            ));
        }
        let mut bases: Vec<i64> = (0..count_i64).map(|index| base + stride * index).collect();
        let slot = usize::try_from(override_index)
            .ok()
            .and_then(|index| bases.get_mut(index))
            .ok_or_else(|| {
                extract_error!("record base override index {override_index} is invalid")
            })?;
        *slot = override_base;
        let mut overrides = serde_json::Map::new();
        drop(overrides.insert(override_index.to_string(), Value::from(override_base)));
        return Ok((
            variable,
            json!({
                "kind": "linear_with_override",
                "base": base,
                "stride": stride,
                "overrides": overrides,
            }),
            bases,
        ));
    }
    Err(extract_error!(
        "unsupported record base expression: {}",
        expression.trim()
    ))
}

/// Offset of a record write relative to its base-offset variable.
fn relative_record_offset(expression: &str, variable: &str) -> Result<i64> {
    let compact = compact_expression(expression);
    if compact == variable {
        return Ok(0);
    }
    let pattern = regex::Regex::new(&format!(r"^{}\+(\d+)$", regex::escape(variable)))
        .map_err(|error| extract_error!("record offset pattern failed to compile: {error}"))?;
    if let Some(capture) = pattern.captures(&compact)
        && let Some(offset) = capture
            .get(1)
            .and_then(|digits| digits.as_str().parse().ok())
    {
        return Ok(offset);
    }
    Err(extract_error!(
        "record write offset is not a non-negative constant relative to {variable}: {expression}"
    ))
}

/// Index of the offset argument for a supported record writer call.
fn record_offset_argument(method: &str, arguments: &[String]) -> Result<usize> {
    if matches!(method, "c" | "d") && arguments.len() == 3 {
        return Ok(1);
    }
    if method == "b" && arguments.len() == 3 {
        return Ok(2);
    }
    if method == "a" && (2..=4).contains(&arguments.len()) {
        return Ok(arguments.len() - 1);
    }
    Err(extract_error!(
        "unsupported record writer call A_0.{method}({})",
        arguments.join(", ")
    ))
}

/// Expand per-record relative fields to absolute indexed offsets.
fn expand_record_fields(list_name: &str, bases: &[i64], fields: &[Value]) -> Result<Vec<Value>> {
    let mut expanded_fields = Vec::new();
    for (record_index, base) in bases.iter().enumerate() {
        for field in fields {
            if req_str(field, "role")? != "field" {
                continue;
            }
            let offset = base + req_i64(field, "relative_offset")?;
            let name = display_name(req(field, "name")?);
            let mut expanded = json!({
                "record_index": record_index,
                "name": format!("{list_name}[{record_index}].{name}"),
                "offset": offset,
                "offset_hex": format!("0x{offset:04X}"),
                "codec": req(field, "codec")?.clone(),
            });
            for key in [
                "aliases",
                "storage_transform",
                "domain",
                "writable",
                "not_writable_reason",
            ] {
                if let Some(extra) = field.get(key) {
                    insert(&mut expanded, key, extra.clone())?;
                }
            }
            expanded_fields.push(expanded);
        }
    }
    Ok(expanded_fields)
}

/// Build one per-record field entry from a single direct writer call.
#[expect(
    clippy::too_many_arguments,
    reason = "single-use helper split out of extract_repeated_record_with for length; the arguments are that function's locals"
)]
fn record_field(
    patterns: &Patterns,
    spec: &RecordSpec,
    types: &crate::sources::ClassTypes,
    symbol_overrides: &HashMap<&str, Value>,
    constants: &HashMap<String, i64>,
    variable: &str,
    sequence: usize,
    writer: &str,
    call_arguments: &str,
) -> Result<Value> {
    let source_class = spec.source_class;
    let method = spec.method;
    let mut arguments = split_arguments(call_arguments);
    let source_expression = arguments
        .first()
        .map(|text| text.trim().to_owned())
        .unwrap_or_default();
    let override_entry = symbol_overrides.get(source_expression.as_str());
    let mut parse_properties = types.properties.clone();
    if let Some(override_value) = override_entry {
        let override_name = req_str(override_value, "name")?.to_owned();
        let override_type = req_str(override_value, "csharp_type")?.to_owned();
        if let Some(first) = arguments.first_mut() {
            first.clone_from(&override_name);
        }
        drop(parse_properties.insert(override_name, override_type));
    }
    let offset_index = record_offset_argument(writer, &arguments)?;
    let offset_expression = arguments.get(offset_index).cloned().unwrap_or_default();
    let relative_offset = relative_record_offset(&offset_expression, variable)?;
    if let Some(slot) = arguments.get_mut(offset_index) {
        *slot = relative_offset.to_string();
    }
    let (mut codec, name, role, parsed_offset) = codec_for_call(
        patterns,
        writer,
        &arguments,
        &parse_properties,
        &types.private_fields,
        &types.enums,
        constants,
    )?;
    if parsed_offset != relative_offset {
        return Err(extract_error!(
            "internal record offset mismatch in {source_class}.{method}: \
             {parsed_offset} versus {relative_offset}"
        ));
    }
    let field_name: Option<String> = match override_entry {
        Some(override_value) => Some(req_str(override_value, "name")?.to_owned()),
        None => name,
    };
    if let Some(field_name) = field_name.as_deref()
        && let Some(padding) = fixed_string_padding_override(source_class, field_name)
        && codec.get("kind").and_then(Value::as_str) == Some("fixed_string")
    {
        insert(&mut codec, "padding", Value::from(padding))?;
    }
    let field_role = override_entry
        .and_then(|override_value| override_value.get("role"))
        .and_then(Value::as_str)
        .unwrap_or(role);
    let mut field = json!({
        "sequence": sequence,
        "role": field_role,
        "name": field_name,
        "relative_offset": relative_offset,
        "codec": without_nulls(&codec)?,
    });
    if let Some(override_value) = override_entry {
        for key in ["aliases", "storage_transform"] {
            if let Some(extra) = override_value.get(key) {
                insert(&mut field, key, extra.clone())?;
            }
        }
    }
    let domain_name: Option<String> = req(&field, "name")?.as_str().map(ToOwned::to_owned);
    if let Some(domain) = domain_name
        .as_deref()
        .and_then(|field_name| crate::tables::RECORD_FIELD_DOMAINS.get(&(source_class, field_name)))
    {
        insert(&mut field, "domain", domain.clone())?;
    }
    if source_class == "MyPositionData"
        && req(&field, "name")?.as_str() == Some("MyPositionChannel")
    {
        insert(&mut field, "writable", Value::from(false))?;
        insert(
            &mut field,
            "not_writable_reason",
            Value::from("public storage-width byte has no verified MCP-D75 UI/domain semantics"),
        )?;
    }
    Ok(field)
}

/// Extract and expand one statically sized public child serializer.
pub(crate) fn extract_repeated_record_with(
    patterns: &Patterns,
    spec: &RecordSpec,
    path: &Path,
    source: &str,
    source_dir: &Path,
    constants: &HashMap<String, i64>,
) -> Result<Value> {
    let source_class = spec.source_class;
    let method = spec.method;
    let count = spec.count;
    let (body, method_line) = find_balanced_body(
        source,
        &format!(
            r"^\s*public\s+(?:override\s+)?void\s+{}\s*\(\s*m6\s+A_0\s*,\s*int\s+A_1\s*\)",
            regex::escape(method)
        ),
    )?;
    let (variable, offset_layout, bases) = record_offset_layout(patterns, &body, count)?;
    let types = parse_types(patterns, source)?;
    let empty = HashMap::new();
    let symbol_overrides: &HashMap<&str, Value> =
        RECORD_SYMBOLS.get(source_class).unwrap_or(&empty);
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
            "{source_class}.{method} has {direct_mention_count} A_0 calls but only {} \
             match the supported one-line call shape",
            direct_matches.len()
        ));
    }

    let mut fields = Vec::new();
    for (sequence, (writer, call_arguments)) in direct_matches.iter().enumerate() {
        fields.push(record_field(
            patterns,
            spec,
            &types,
            symbol_overrides,
            constants,
            &variable,
            sequence,
            writer,
            call_arguments,
        )?);
    }

    let expanded_fields = expand_record_fields(spec.name, &bases, &fields)?;
    let field_count_per_record = fields
        .iter()
        .map(|field| req_str(field, "role").map(|role| u64::from(role == "field")))
        .sum::<Result<u64>>()?;
    let relative_path = source_label(path, source_dir);
    Ok(json!({
        "name": spec.name,
        "source_class": source_class,
        "source_file": relative_path,
        "write_method": format!("{method}(m6 A_0, int A_1)"),
        "write_method_line": method_line,
        "count": count,
        "offset_layout": offset_layout,
        "record_base_offsets": bases,
        "operation_count_per_record": fields.len(),
        "field_count_per_record": field_count_per_record,
        "fields": fields,
        "expanded_fields": expanded_fields,
    }))
}
