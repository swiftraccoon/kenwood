//! Classification of the serializer's direct `A_0.<method>(...)` writes.
//!
//! The memory writer exposes the same overload family on both radios:
//! `a(byte|bool|byte[], offset)`, `a(int, int)` (clear), `a(value, bit, offset)`,
//! `a(value, bit, width, offset)`, `a(uint, width, offset)`, `b(int, width, offset)`,
//! `c(string, offset, len)` (memory-map encoding), `d(string, offset, len)` (UTF-8).

use std::collections::HashMap;

use crate::address::{SlotSymbol, parse_affine};
use crate::csharp::Patterns;
use crate::error::{Result, extract_error};
use crate::manifest::{Codec, Role};
use crate::model::ValueHelperSpec;
use crate::sources::{ClassTypes, resolve_integer};

/// A classified direct write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classified {
    /// On-image codec.
    pub(crate) codec: Codec,
    /// Written property or field name.
    pub(crate) name: Option<String>,
    /// Field, internal, constant, or clear.
    pub(crate) role: Role,
    /// Offset argument as written, for the caller to resolve.
    pub(crate) offset_expression: String,
}

/// What a written expression refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValueMeta {
    /// Bare identifier, when the expression names one.
    pub(crate) identifier: Option<String>,
    /// Declared C# type of that identifier.
    pub(crate) csharp_type: Option<String>,
    /// `bool`, `byte`, `bytes`, `string`, `int`, `uint`, `enum`, `constant`, or `unknown`.
    pub(crate) value_type: String,
    /// Qualified `<declaring class>.<enum>` name when the value is an enum.
    pub(crate) enum_type: Option<String>,
    /// Trimmed source expression.
    pub(crate) normalized: String,
    /// Value helper wrapping the identifier, when the spec pins one.
    pub(crate) helper: Option<&'static ValueHelperSpec>,
}

fn identifier_of(patterns: &Patterns, text: &str) -> Option<String> {
    patterns
        .identifier_re
        .captures(text)
        .and_then(|capture| capture.get(1))
        .map(|name| name.as_str().to_owned())
}

/// Unwrap casts, `Convert.ToByte`, and spec'd helpers around an identifier.
fn unwrap_expression(
    patterns: &Patterns,
    expression: &str,
    helpers: &'static [ValueHelperSpec],
) -> (
    Option<String>,
    Option<String>,
    Option<&'static ValueHelperSpec>,
    String,
) {
    let normalized = expression.trim().to_owned();
    if let Some(helper) = patterns.convert_to_byte_re.captures(&normalized) {
        let inner = helper.get(1).map(|m| m.as_str()).unwrap_or_default().trim();
        return (
            identifier_of(patterns, inner),
            Some("Convert.ToByte".to_owned()),
            None,
            normalized,
        );
    }
    if let Some(cast) = patterns.cast_re.captures(&normalized) {
        let cast_type = cast
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let inner = cast.get(2).map(|m| m.as_str()).unwrap_or_default().trim();
        return (
            identifier_of(patterns, inner),
            Some(cast_type),
            None,
            normalized,
        );
    }
    if let Some(call) = patterns.helper_call_re.captures(&normalized) {
        let inner = call.get(2).map(|m| m.as_str()).unwrap_or_default();
        let identifier = identifier_of(patterns, inner);
        let helper = identifier
            .as_deref()
            .and_then(|name| helpers.iter().find(|helper| helper.property == name));
        if helper.is_some() {
            return (identifier, None, helper, normalized);
        }
    }
    (identifier_of(patterns, &normalized), None, None, normalized)
}

/// Classify a written expression.
pub(crate) fn value_metadata(
    patterns: &Patterns,
    expression: &str,
    types: &ClassTypes,
    helpers: &'static [ValueHelperSpec],
) -> ValueMeta {
    let (identifier, cast, helper, normalized) = unwrap_expression(patterns, expression, helpers);
    let lookup = identifier.as_deref().unwrap_or("");
    let csharp_type = types
        .properties
        .get(lookup)
        .or_else(|| types.private_fields.get(lookup))
        .cloned();
    let enum_type = csharp_type
        .as_deref()
        .and_then(|kind| types.enums.get(kind))
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
            Some(_) if enum_type.is_some() => "enum".to_owned(),
            _ if identifier.is_none() => "constant".to_owned(),
            _ => "unknown".to_owned(),
        }
    };
    ValueMeta {
        identifier,
        csharp_type,
        value_type,
        enum_type,
        normalized,
        helper,
    }
}

/// Role of a written name.
pub(crate) fn value_role(name: Option<&str>, types: &ClassTypes) -> Role {
    match name {
        Some(name) if types.properties.contains_key(name) => Role::Field,
        Some(name) if types.private_fields.contains_key(name) => Role::Internal,
        _ => Role::Constant,
    }
}

fn resolves_to_integer(expression: &str, constants: &HashMap<String, i64>) -> bool {
    let trimmed = expression.trim();
    parse_affine(trimmed)
        .is_ok_and(|affine| affine.index_stride.is_none() && affine.symbols.is_empty())
        || constants.contains_key(trimmed)
}

fn is_clear_offset(
    expression: &str,
    constants: &HashMap<String, i64>,
    slots: &[SlotSymbol],
) -> bool {
    if resolves_to_integer(expression, constants) {
        return true;
    }
    parse_affine(expression).is_ok_and(|affine| {
        affine.index_stride.is_none()
            && !affine.symbols.is_empty()
            && affine.symbols.iter().all(|symbol| {
                slots.iter().any(|slot| slot.symbol == *symbol) || constants.contains_key(symbol)
            })
    })
}

fn constant_expression(meta: &ValueMeta, role: Role) -> Option<String> {
    (role == Role::Constant).then(|| meta.normalized.clone())
}

fn classify_string(
    patterns: &Patterns,
    method: &str,
    args: &[String],
    types: &ClassTypes,
    constants: &HashMap<String, i64>,
    helpers: &'static [ValueHelperSpec],
) -> Result<Classified> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    let meta = value_metadata(patterns, arg(0), types, helpers);
    let length = resolve_integer(patterns, arg(2), constants).ok_or_else(|| {
        extract_error!(
            "unresolved string length: A_0.{method}({})",
            args.join(", ")
        )
    })?;
    let length = u64::try_from(length)
        .map_err(|_| extract_error!("negative string length: A_0.{method}({})", args.join(", ")))?;
    let role = value_role(meta.identifier.as_deref(), types);
    Ok(Classified {
        codec: Codec::FixedString {
            encoding: if method == "c" { "memory_map" } else { "utf8" }.to_owned(),
            length,
            padding: 0,
            csharp_type: meta
                .csharp_type
                .clone()
                .unwrap_or_else(|| "string".to_owned()),
            value_type: meta.value_type.clone(),
        },
        name: meta.identifier,
        role,
        offset_expression: arg(1).trim().to_owned(),
    })
}

fn classify_signed(
    patterns: &Patterns,
    args: &[String],
    types: &ClassTypes,
    constants: &HashMap<String, i64>,
    helpers: &'static [ValueHelperSpec],
) -> Result<Classified> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    let meta = value_metadata(patterns, arg(0), types, helpers);
    let width = resolve_integer(patterns, arg(1), constants)
        .and_then(|width| u64::try_from(width).ok())
        .ok_or_else(|| {
            extract_error!(
                "unresolved signed integer write: A_0.b({})",
                args.join(", ")
            )
        })?;
    // The writer truncates BitConverter.GetBytes(int) to `width`; its matching
    // reader copies into a zero-filled Int32, so widths below four are
    // zero-extended unsigned storage and only width four keeps the sign.
    let csharp_type = meta.csharp_type.clone().unwrap_or_else(|| "int".to_owned());
    let codec = if width == 4 {
        Codec::SignedLe {
            width,
            csharp_type,
            value_type: meta.value_type.clone(),
            enum_type: meta.enum_type.clone(),
        }
    } else {
        Codec::UnsignedLe {
            width,
            csharp_type,
            value_type: meta.value_type.clone(),
            enum_type: meta.enum_type.clone(),
        }
    };
    let role = value_role(meta.identifier.as_deref(), types);
    Ok(Classified {
        codec,
        name: meta.identifier,
        role,
        offset_expression: arg(2).trim().to_owned(),
    })
}

fn classify_two_arg_a(
    patterns: &Patterns,
    args: &[String],
    types: &ClassTypes,
    constants: &HashMap<String, i64>,
    slots: &[SlotSymbol],
    helpers: &'static [ValueHelperSpec],
) -> Result<Classified> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    // The int,int overload clears a range. A cast such as (byte)0 selects
    // the byte writer instead, so a clear needs both arguments to be offsets.
    if is_clear_offset(arg(0), constants, slots) && resolves_to_integer(arg(1), constants) {
        let length = resolve_integer(patterns, arg(1), constants)
            .and_then(|length| u64::try_from(length).ok())
            .ok_or_else(|| extract_error!("unresolved clear length: A_0.a({})", args.join(", ")))?;
        return Ok(Classified {
            codec: Codec::ClearRange { length, fill: 255 },
            name: None,
            role: Role::Clear,
            offset_expression: arg(0).trim().to_owned(),
        });
    }
    let meta = value_metadata(patterns, arg(0), types, helpers);
    let role = value_role(meta.identifier.as_deref(), types);
    let value_expression = constant_expression(&meta, role);
    let codec = if let Some(helper) = meta.helper {
        Codec::RawBytes {
            csharp_type: meta.csharp_type.clone(),
            value_type: meta.value_type.clone(),
            length: Some(helper.length),
            encoding: Some(helper.encoding.to_owned()),
            value_expression,
        }
    } else {
        match meta.value_type.as_str() {
            "bytes" => Codec::RawBytes {
                csharp_type: meta.csharp_type.clone(),
                value_type: meta.value_type.clone(),
                length: None,
                encoding: None,
                value_expression,
            },
            "bool" => Codec::Bool {
                csharp_type: meta.csharp_type.clone(),
                value_type: meta.value_type.clone(),
                value_expression,
            },
            _ => Codec::Byte {
                csharp_type: meta.csharp_type.clone(),
                value_type: meta.value_type.clone(),
                value_expression,
                enum_type: meta.enum_type.clone(),
            },
        }
    };
    Ok(Classified {
        codec,
        name: meta.identifier,
        role,
        offset_expression: arg(1).trim().to_owned(),
    })
}

fn classify_three_arg_a(
    patterns: &Patterns,
    args: &[String],
    types: &ClassTypes,
    constants: &HashMap<String, i64>,
    helpers: &'static [ValueHelperSpec],
) -> Result<Classified> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    let meta = value_metadata(patterns, arg(0), types, helpers);
    let middle = resolve_integer(patterns, arg(1), constants)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| {
            extract_error!("unresolved bit/unsigned write: A_0.a({})", args.join(", "))
        })?;
    let role = value_role(meta.identifier.as_deref(), types);
    let value_expression = constant_expression(&meta, role);
    // Every 3-argument a() in the known serializers is the byte,bit,offset
    // overload; a uint property selects uint,width,offset instead.
    let codec = if meta.csharp_type.as_deref() == Some("uint") {
        Codec::UnsignedLe {
            width: middle,
            csharp_type: "uint".to_owned(),
            value_type: meta.value_type.clone(),
            enum_type: meta.enum_type.clone(),
        }
    } else {
        Codec::BitField {
            bit: middle,
            width: 1,
            csharp_type: meta.csharp_type.clone(),
            value_type: meta.value_type.clone(),
            value_expression,
            enum_type: meta.enum_type.clone(),
        }
    };
    Ok(Classified {
        codec,
        name: meta.identifier,
        role,
        offset_expression: arg(2).trim().to_owned(),
    })
}

fn classify_four_arg_a(
    patterns: &Patterns,
    args: &[String],
    types: &ClassTypes,
    constants: &HashMap<String, i64>,
    helpers: &'static [ValueHelperSpec],
) -> Result<Classified> {
    let arg = |index: usize| -> &str { args.get(index).map(String::as_str).unwrap_or_default() };
    let meta = value_metadata(patterns, arg(0), types, helpers);
    let coordinate = |index: usize| -> Result<u64> {
        resolve_integer(patterns, arg(index), constants)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| extract_error!("unresolved bit-field write: A_0.a({})", args.join(", ")))
    };
    let (bit, width) = (coordinate(1)?, coordinate(2)?);
    let role = value_role(meta.identifier.as_deref(), types);
    let value_expression = constant_expression(&meta, role);
    Ok(Classified {
        codec: Codec::BitField {
            bit,
            width,
            csharp_type: meta.csharp_type.clone(),
            value_type: meta.value_type.clone(),
            value_expression,
            enum_type: meta.enum_type.clone(),
        },
        name: meta.identifier,
        role,
        offset_expression: arg(3).trim().to_owned(),
    })
}

/// Classify one direct writer call.
pub(crate) fn classify_call(
    patterns: &Patterns,
    method: &str,
    args: &[String],
    types: &ClassTypes,
    constants: &HashMap<String, i64>,
    slots: &[SlotSymbol],
    helpers: &'static [ValueHelperSpec],
) -> Result<Classified> {
    match (method, args.len()) {
        ("c" | "d", 3) => classify_string(patterns, method, args, types, constants, helpers),
        ("b", 3) => classify_signed(patterns, args, types, constants, helpers),
        ("a", 2) => classify_two_arg_a(patterns, args, types, constants, slots, helpers),
        ("a", 3) => classify_three_arg_a(patterns, args, types, constants, helpers),
        ("a", 4) => classify_four_arg_a(patterns, args, types, constants, helpers),
        ("a", arity) => Err(extract_error!("unsupported A_0.a arity {arity}: {args:?}")),
        _ => Err(extract_error!(
            "unsupported direct writer method A_0.{method}"
        )),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::csharp::split_arguments;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    static IPV4: [ValueHelperSpec; 1] = [ValueHelperSpec {
        property: "IpAddress",
        length: 4,
        encoding: "ipv4_dotted_quad",
    }];

    fn types() -> ClassTypes {
        ClassTypes {
            properties: HashMap::from([
                ("BeatShift".to_owned(), "a".to_owned()),
                ("TxInhibit".to_owned(), "bool".to_owned()),
                ("PowerOnMessage".to_owned(), "string".to_owned()),
                ("Interval".to_owned(), "int".to_owned()),
                ("IpAddress".to_owned(), "string".to_owned()),
                ("PoweronBitmap".to_owned(), "byte[]".to_owned()),
            ]),
            private_fields: HashMap::from([("an".to_owned(), "byte".to_owned())]),
            enums: HashMap::from([("a".to_owned(), "m9.a".to_owned())]),
        }
    }

    fn classify(call: &str) -> std::result::Result<Classified, Box<dyn std::error::Error>> {
        let patterns = Patterns::new()?;
        let (method, rest) = call.split_once('(').ok_or("bad call")?;
        let args = split_arguments(rest.trim_end_matches(')'));
        let constants = HashMap::from([("nb.c".to_owned(), 16_i64)]);
        let slots = [SlotSymbol {
            symbol: "g".to_owned(),
            anchor: "OffsetProgrammableMemoryBitmapAddress".to_owned(),
            dimension: "pm_slot".to_owned(),
            stride: 256_000,
        }];
        Ok(classify_call(
            &patterns,
            method,
            &args,
            &types(),
            &constants,
            &slots,
            &IPV4,
        )?)
    }

    #[test]
    fn classifies_the_writer_overloads() -> TestResult {
        let enum_write = classify("a((byte)BeatShift, 4096)")?;
        assert_eq!(enum_write.role, Role::Field);
        assert_eq!(enum_write.codec.value_type(), Some("enum"));
        assert_eq!(enum_write.codec.enum_type(), Some("m9.a"));
        assert_eq!(enum_write.offset_expression, "4096");
        let bit = classify("a(Convert.ToByte(TxInhibit), 0, 4136)")?;
        assert!(
            matches!(
                bit.codec,
                Codec::BitField {
                    bit: 0,
                    width: 1,
                    ..
                }
            ),
            "{bit:?}"
        );
        let text = classify("c(PowerOnMessage, 4288, nb.c)")?;
        assert!(
            matches!(text.codec, Codec::FixedString { length: 16, .. }),
            "{text:?}"
        );
        let signed = classify("b(Interval, 2, 4368)")?;
        assert!(
            matches!(signed.codec, Codec::UnsignedLe { width: 2, .. }),
            "{signed:?}"
        );
        let internal = classify("a(an, 6690)")?;
        assert_eq!(internal.role, Role::Internal);
        let constant = classify("a((byte)0, 4199)")?;
        assert_eq!(constant.role, Role::Constant);
        assert!(
            matches!(
                constant.codec,
                Codec::Byte {
                    value_expression: Some(_),
                    ..
                }
            ),
            "{constant:?}"
        );
        Ok(())
    }

    #[test]
    fn classifies_clears_including_slot_relative_ones() -> TestResult {
        let literal = classify("a(327680, 86400)")?;
        assert_eq!(literal.role, Role::Clear);
        assert_eq!(literal.offset_expression, "327680");
        let slot_relative = classify("a(393216 + g, 86400)")?;
        assert_eq!(slot_relative.role, Role::Clear);
        assert_eq!(slot_relative.offset_expression, "393216 + g");
        let bytes = classify("a(PoweronBitmap, 393216 + g)")?;
        assert!(
            matches!(bytes.codec, Codec::RawBytes { length: None, .. }),
            "{bytes:?}"
        );
        Ok(())
    }

    #[test]
    fn helper_wrapped_values_use_the_spec_codec() -> TestResult {
        let helper = classify("a(a(IpAddress), 332856 + c)")?;
        assert_eq!(helper.name.as_deref(), Some("IpAddress"));
        assert_eq!(helper.role, Role::Field);
        assert!(
            matches!(&helper.codec, Codec::RawBytes { length: Some(4), encoding: Some(encoding), .. } if encoding == "ipv4_dotted_quad"),
            "{helper:?}"
        );
        Ok(())
    }
}
