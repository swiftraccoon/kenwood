//! Classification of the serializer's direct `A_0.<method>(...)` writes.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::csharp::Patterns;
use crate::error::{Result, extract_error};
use crate::sources::resolve_integer;

/// Return `(identifier, explicit cast/helper, normalized expression)`.
pub(crate) fn unwrap_expression(
    patterns: &Patterns,
    expression: &str,
) -> (Option<String>, Option<String>, String) {
    let normalized = expression.trim().to_owned();
    let identifier_of = |text: &str| -> Option<String> {
        patterns
            .identifier_re
            .captures(text)
            .and_then(|capture| capture.get(1))
            .map(|name| name.as_str().to_owned())
    };
    if let Some(helper) = patterns.convert_to_byte_re.captures(&normalized) {
        let inner = helper
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .trim()
            .to_owned();
        return (
            identifier_of(&inner),
            Some("Convert.ToByte".to_owned()),
            normalized,
        );
    }
    if let Some(cast) = patterns.cast_re.captures(&normalized) {
        let cast_type = cast
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let inner = cast
            .get(2)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .trim()
            .to_owned();
        return (identifier_of(&inner), Some(cast_type), normalized);
    }
    (identifier_of(&normalized), None, normalized)
}

/// Classify a written expression: identifier, C# type, value type.
pub(crate) fn value_metadata(
    patterns: &Patterns,
    expression: &str,
    properties: &HashMap<String, String>,
    private_fields: &HashMap<String, String>,
    enums: &HashSet<String>,
) -> (Option<String>, Option<String>, String, String) {
    let (identifier, cast, normalized) = unwrap_expression(patterns, expression);
    let lookup = identifier.as_deref().unwrap_or("");
    let csharp_type = properties
        .get(lookup)
        .or_else(|| private_fields.get(lookup))
        .cloned();
    let value_type = if cast.as_deref() == Some("byte") && identifier.is_none() {
        "constant".to_owned()
    } else {
        match csharp_type.as_deref() {
            Some("bool") => "bool".to_owned(),
            Some("byte") => "byte".to_owned(),
            Some("byte[]") => "bytes".to_owned(),
            Some("string") => "string".to_owned(),
            Some(kind @ ("int" | "uint")) => kind.to_owned(),
            Some(kind) if enums.contains(kind) => "enum".to_owned(),
            _ if identifier.is_none() => "constant".to_owned(),
            _ => "unknown".to_owned(),
        }
    };
    (identifier, csharp_type, value_type, normalized)
}

/// Role of a written name: public field, internal state, or constant.
pub(crate) fn value_role(
    name: Option<&str>,
    properties: &HashMap<String, String>,
    private_fields: &HashMap<String, String>,
) -> &'static str {
    match name {
        Some(name) if properties.contains_key(name) => "field",
        Some(name) if private_fields.contains_key(name) => "internal",
        _ => "constant",
    }
}

/// A classified write call: codec object, field name, role, and offset.
pub(crate) type ClassifiedCall = (Value, Option<String>, &'static str, i64);

/// Return `(codec, field name, role, offset)` for one direct writer call.
pub(crate) fn codec_for_call(
    patterns: &Patterns,
    method: &str,
    args: &[String],
    properties: &HashMap<String, String>,
    private_fields: &HashMap<String, String>,
    enums: &HashSet<String>,
    constants: &HashMap<String, i64>,
) -> Result<ClassifiedCall> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    if matches!(method, "c" | "d") && args.len() == 3 {
        let (name, csharp_type, value_type, _) =
            value_metadata(patterns, arg(0), properties, private_fields, enums);
        let offset = resolve_integer(patterns, arg(1), constants);
        let length = resolve_integer(patterns, arg(2), constants);
        let (Some(offset), Some(length)) = (offset, length) else {
            return Err(extract_error!(
                "unresolved string offset/length: A_0.{method}({})",
                args.join(", ")
            ));
        };
        let encoding = if method == "c" { "memory_map" } else { "utf8" };
        let codec = json!({
            "kind": "fixed_string",
            "encoding": encoding,
            "length": length,
            "padding": 0,
            "csharp_type": csharp_type.unwrap_or_else(|| "string".to_owned()),
            "value_type": value_type,
        });
        let role = value_role(name.as_deref(), properties, private_fields);
        return Ok((codec, name, role, offset));
    }

    if method == "b" && args.len() == 3 {
        let (name, csharp_type, value_type, _) =
            value_metadata(patterns, arg(0), properties, private_fields, enums);
        let width = resolve_integer(patterns, arg(1), constants);
        let offset = resolve_integer(patterns, arg(2), constants);
        let (Some(width), Some(offset)) = (width, offset) else {
            return Err(extract_error!(
                "unresolved signed integer write: A_0.b({})",
                args.join(", ")
            ));
        };
        // m6.b truncates BitConverter.GetBytes(int) to `width`, while its
        // matching h(offset, width) reader copies into a zero-filled Int32.
        // Widths below four are therefore zero-extended unsigned storage;
        // width four preserves the signed Int32 bit pattern.
        let kind = if width == 4 {
            "signed_le"
        } else {
            "unsigned_le"
        };
        let codec = json!({
            "kind": kind,
            "width": width,
            "csharp_type": csharp_type.unwrap_or_else(|| "int".to_owned()),
            "value_type": value_type,
        });
        let role = value_role(name.as_deref(), properties, private_fields);
        return Ok((codec, name, role, offset));
    }

    if method != "a" {
        return Err(extract_error!(
            "unsupported direct writer method A_0.{method}"
        ));
    }
    codec_for_a_call(patterns, args, properties, private_fields, enums, constants)
}

/// Decode the overloaded `A_0.a` writer: byte, bool, clear, bit, or uint.
pub(crate) fn codec_for_a_call(
    patterns: &Patterns,
    args: &[String],
    properties: &HashMap<String, String>,
    private_fields: &HashMap<String, String>,
    enums: &HashSet<String>,
    constants: &HashMap<String, i64>,
) -> Result<ClassifiedCall> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    if args.len() == 2 {
        let first_number = resolve_integer(patterns, arg(0), constants);
        let second_number = resolve_integer(patterns, arg(1), constants);
        // The int,int overload clears a range.  A cast such as (byte)0 selects
        // the byte writer instead, and therefore must not be treated as a clear.
        if let (Some(first), Some(second)) = (first_number, second_number) {
            let codec = json!({"kind": "clear_range", "length": second, "fill": 255});
            return Ok((codec, None, "clear", first));
        }
        let Some(offset) = second_number else {
            return Err(extract_error!(
                "unresolved byte offset: A_0.a({})",
                args.join(", ")
            ));
        };
        let (name, csharp_type, value_type, normalized) =
            value_metadata(patterns, arg(0), properties, private_fields, enums);
        let kind = match value_type.as_str() {
            "bytes" => "raw_bytes",
            "bool" => "bool",
            _ => "byte",
        };
        let mut codec = json!({
            "kind": kind,
            "csharp_type": csharp_type,
            "value_type": value_type,
        });
        let role = value_role(name.as_deref(), properties, private_fields);
        if role == "constant"
            && let Some(map) = codec.as_object_mut()
        {
            drop(map.insert("value_expression".to_owned(), Value::from(normalized)));
        }
        return Ok((codec, name, role, offset));
    }

    if args.len() == 3 {
        let (name, csharp_type, value_type, normalized) =
            value_metadata(patterns, arg(0), properties, private_fields, enums);
        let middle = resolve_integer(patterns, arg(1), constants);
        let offset = resolve_integer(patterns, arg(2), constants);
        let (Some(middle), Some(offset)) = (middle, offset) else {
            return Err(extract_error!(
                "unresolved bit/unsigned write: A_0.a({})",
                args.join(", ")
            ));
        };
        // Every 3-argument a() in the four known serializers is the byte,bit,
        // offset overload.  A uint property would instead select uint,width,
        // offset; retain support for that signature for future versions.
        let mut codec = if csharp_type.as_deref() == Some("uint") {
            json!({
                "kind": "unsigned_le",
                "width": middle,
                "csharp_type": csharp_type,
                "value_type": value_type,
            })
        } else {
            json!({
                "kind": "bit_field",
                "bit": middle,
                "width": 1,
                "csharp_type": csharp_type,
                "value_type": value_type,
            })
        };
        let role = value_role(name.as_deref(), properties, private_fields);
        if role == "constant"
            && let Some(map) = codec.as_object_mut()
        {
            drop(map.insert("value_expression".to_owned(), Value::from(normalized)));
        }
        return Ok((codec, name, role, offset));
    }

    if args.len() == 4 {
        let (name, csharp_type, value_type, normalized) =
            value_metadata(patterns, arg(0), properties, private_fields, enums);
        let bit = resolve_integer(patterns, arg(1), constants);
        let width = resolve_integer(patterns, arg(2), constants);
        let offset = resolve_integer(patterns, arg(3), constants);
        let (Some(bit), Some(width), Some(offset)) = (bit, width, offset) else {
            return Err(extract_error!(
                "unresolved bit-field write: A_0.a({})",
                args.join(", ")
            ));
        };
        let mut codec = json!({
            "kind": "bit_field",
            "bit": bit,
            "width": width,
            "csharp_type": csharp_type,
            "value_type": value_type,
        });
        let role = value_role(name.as_deref(), properties, private_fields);
        if role == "constant"
            && let Some(map) = codec.as_object_mut()
        {
            drop(map.insert("value_expression".to_owned(), Value::from(normalized)));
        }
        return Ok((codec, name, role, offset));
    }

    Err(extract_error!(
        "unsupported A_0.a arity {}: {args:?}",
        args.len()
    ))
}
