//! Public repeated-record lists and the private sub-writer catalog (spec D5).

use crate::address::{
    Address, RecordBase, SlotSymbol, SymbolScope, Term, resolve_offset, resolve_record_base,
};
use crate::class_index::{ClassIndex, ClassInfo};
use crate::codecs::classify_call;
use crate::csharp::{compact_expression, split_arguments};
use crate::discovery::{
    MethodRef, NestedCall, child_writer, find_base_override, find_writer, resolve_list_target,
    setter_symbol, slot_symbols, verify_anchor_passthrough,
};
use crate::error::{Result, extract_error};
use crate::manifest::{
    Codec, ExpandedField, OffsetLayout, PrivateRecord, Record, RecordField, Role, offset_hex,
};
use crate::model::{PrivateWriterSpec, RecordSpec, StorageTransformSpec, SymbolOverride};
use crate::operations::{WriteScope, direct_calls};
use crate::sources::ClassTypes;

/// The class whose writer makes the nested calls, with its slot symbols.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Owner<'a> {
    /// Project class index.
    pub(crate) index: &'a ClassIndex,
    /// Calling class.
    pub(crate) class: &'a ClassInfo,
    /// Slot symbols of the calling class.
    pub(crate) slots: &'a [SlotSymbol],
    /// Memory writer class name.
    pub(crate) writer_class: &'a str,
}

/// The single `int <var> = <expr containing A_1>;` assignment of a child writer.
fn base_assignment(scope: &WriteScope<'_>, body: &str, label: &str) -> Result<(String, String)> {
    let assignments: Vec<(String, String)> = scope
        .patterns
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
    match assignments.as_slice() {
        [(variable, expression)] => Ok((variable.clone(), expression.clone())),
        _ => Err(extract_error!(
            "{label} must contain exactly one A_1 base assignment, got {assignments:?}"
        )),
    }
}

/// Offset of a record write relative to its base variable.
fn relative_record_offset(expression: &str, variable: &str) -> Result<u64> {
    let compact = compact_expression(expression);
    if compact == variable {
        return Ok(0);
    }
    compact
        .strip_prefix(variable)
        .and_then(|rest| rest.strip_prefix('+'))
        .and_then(|digits| digits.parse::<u64>().ok())
        .ok_or_else(|| {
            extract_error!(
                "record write offset is not a non-negative constant relative to {variable}: {expression}"
            )
        })
}

/// Index of the offset argument for a supported record writer call.
fn record_offset_argument(method: &str, arguments: &[String]) -> Result<usize> {
    match (method, arguments.len()) {
        ("c" | "d", 3) => Ok(1),
        ("b", 3) => Ok(2),
        ("a", 2..=4) => Ok(arguments.len() - 1),
        _ => Err(extract_error!(
            "unsupported record writer call A_0.{method}({})",
            arguments.join(", ")
        )),
    }
}

fn layout_from(base: &RecordBase) -> OffsetLayout {
    OffsetLayout {
        kind: if base.overrides.is_empty() {
            "linear"
        } else {
            "linear_with_override"
        }
        .to_owned(),
        base: base.base,
        stride: Some(base.stride),
        overrides: base
            .overrides
            .iter()
            .map(|(index, value)| (index.to_string(), *value))
            .collect(),
        terms: base.terms.clone(),
    }
}

/// Slot symbols of `target` plus verification that the owner passes its own
/// symbols through to them via the private `field` that holds the target.
fn inherited_slots(
    scope: &WriteScope<'_>,
    owner: &Owner<'_>,
    field: &str,
    target: &ClassInfo,
) -> Result<Vec<SlotSymbol>> {
    let slots = slot_symbols(scope.patterns, target, scope.spec)?;
    for slot in &slots {
        let owner_symbol = owner
            .slots
            .iter()
            .find(|candidate| candidate.anchor == slot.anchor)
            .ok_or_else(|| {
                extract_error!(
                    "{} declares anchor {} but its owner {} has no symbol for it",
                    target.name,
                    slot.anchor,
                    owner.class.name
                )
            })?;
        verify_anchor_passthrough(owner.class, field, &slot.anchor, &owner_symbol.symbol)?;
    }
    Ok(slots)
}

struct FieldContext<'a> {
    scope: &'a WriteScope<'a>,
    record_class: &'a ClassInfo,
    types: &'a ClassTypes,
    symbols: &'static [SymbolOverride],
    variable: &'a str,
}

fn role_override(role: &str) -> Role {
    match role {
        "internal" => Role::Internal,
        "constant" => Role::Constant,
        _ => Role::Field,
    }
}

fn record_field(
    context: &FieldContext<'_>,
    sequence: u64,
    writer: &str,
    argument_text: &str,
) -> Result<RecordField> {
    let mut arguments = split_arguments(argument_text);
    let source_expression = arguments
        .first()
        .map(|text| text.trim().to_owned())
        .unwrap_or_default();
    let override_entry = context
        .symbols
        .iter()
        .find(|symbol| symbol.symbol == source_expression);
    let mut parse_properties = context.types.clone();
    if let Some(entry) = override_entry {
        if let Some(first) = arguments.first_mut() {
            entry.name.clone_into(first);
        }
        drop(
            parse_properties
                .properties
                .insert(entry.name.to_owned(), entry.csharp_type.to_owned()),
        );
    }
    let offset_index = record_offset_argument(writer, &arguments)?;
    let offset_expression = arguments.get(offset_index).cloned().unwrap_or_default();
    let relative_offset = relative_record_offset(&offset_expression, context.variable)?;
    if let Some(slot) = arguments.get_mut(offset_index) {
        *slot = relative_offset.to_string();
    }
    let classified = classify_call(
        context.scope.patterns,
        writer,
        &arguments,
        &parse_properties,
        context.scope.constants,
        &[],
        context.scope.spec.value_helpers,
    )?;
    if classified.codec.value_type() == Some("unknown") {
        return Err(extract_error!(
            "{}: the value of A_0.{writer}({argument_text}) is neither a typed member nor a spec'd record symbol",
            context.record_class.name
        ));
    }
    let mut codec = classified.codec;
    let name = override_entry.map_or(classified.name, |entry| Some(entry.name.to_owned()));
    if let (Some(field_name), Codec::FixedString { padding, .. }) = (name.as_deref(), &mut codec)
        && let Some(fill) = context
            .scope
            .spec
            .padding_override(&context.record_class.name, field_name)
    {
        *padding = fill;
    }
    let role = override_entry
        .and_then(|entry| entry.role)
        .map_or(classified.role, role_override);
    let domain = name.as_deref().and_then(|field_name| {
        context
            .scope
            .spec
            .record_domain(&context.record_class.name, field_name)
    });
    let (writable, not_writable_reason) = override_entry
        .and_then(|entry| entry.not_writable_reason)
        .map_or((None, None), |reason| {
            (Some(false), Some(reason.to_owned()))
        });
    Ok(RecordField {
        sequence,
        role,
        name,
        relative_offset,
        codec,
        aliases: override_entry
            .filter(|entry| !entry.aliases.is_empty())
            .map(|entry| {
                entry
                    .aliases
                    .iter()
                    .map(|alias| (*alias).to_owned())
                    .collect()
            }),
        storage_transform: override_entry.and_then(|entry| {
            entry
                .storage_transform
                .map(StorageTransformSpec::to_manifest)
        }),
        domain,
        writable,
        not_writable_reason,
    })
}

fn expand(
    list: &str,
    record_class: &str,
    bases: &[u64],
    terms: &[Term],
    fields: &[RecordField],
) -> Result<Vec<ExpandedField>> {
    let mut expanded = Vec::new();
    for (record_index, base) in bases.iter().enumerate() {
        for field in fields.iter().filter(|field| field.role == Role::Field) {
            let offset = base
                .checked_add(field.relative_offset)
                .ok_or_else(|| extract_error!("expanded record offset overflows"))?;
            let name = field.name.clone().unwrap_or_else(|| "None".to_owned());
            expanded.push(ExpandedField {
                record_index: u64::try_from(record_index)
                    .map_err(|_| extract_error!("record index overflow"))?,
                name: format!("{list}[{record_index}].{name}"),
                writer_class: record_class.to_owned(),
                offset,
                offset_hex: offset_hex(offset),
                address: Address {
                    base: offset,
                    terms: terms.to_vec(),
                },
                codec: field.codec.clone(),
                aliases: field.aliases.clone(),
                storage_transform: field.storage_transform.clone(),
                domain: field.domain.clone(),
                writable: field.writable,
                not_writable_reason: field.not_writable_reason.clone(),
            });
        }
    }
    Ok(expanded)
}

fn pinned_overrides(
    scope: &WriteScope<'_>,
    owner: &Owner<'_>,
    spec: &RecordSpec,
    field: &str,
    record_class: &ClassInfo,
) -> Result<Vec<(String, u64)>> {
    let Some(base_override) = spec.base_override else {
        return Ok(Vec::new());
    };
    let literal = find_base_override(owner.class, field, base_override.property)?;
    if literal != base_override.value {
        return Err(extract_error!(
            "{}.{field} assigns {} = {literal}, spec pins {}",
            owner.class.name,
            base_override.property,
            base_override.value
        ));
    }
    let symbol =
        setter_symbol(scope.patterns, record_class, base_override.property)?.ok_or_else(|| {
            extract_error!(
                "{} has no {} setter",
                record_class.name,
                base_override.property
            )
        })?;
    Ok(vec![(symbol, literal)])
}

/// Extract one public record list called from the owner's writer.
pub(crate) fn extract_record(
    scope: &WriteScope<'_>,
    owner: &Owner<'_>,
    spec: &RecordSpec,
    call: &NestedCall,
) -> Result<Record> {
    let target = resolve_list_target(scope.patterns, owner.class, &call.target)?;
    if target.property != spec.list {
        return Err(extract_error!(
            "{}: call {} resolves to {} but the spec entry is {}",
            owner.class.name,
            call.target,
            target.property,
            spec.list
        ));
    }
    let record_class = owner
        .index
        .resolve(owner.class, &target.element_class)
        .ok_or_else(|| extract_error!("record class {} not found", target.element_class))?;
    let write = child_writer(
        scope.patterns,
        record_class,
        &call.method,
        owner.writer_class,
    )?;
    let slots = inherited_slots(scope, owner, &target.field, record_class)?;
    let overrides = pinned_overrides(scope, owner, spec, &target.field, record_class)?;
    let label = format!("{}.{}", record_class.name, write.method);
    let (variable, expression) = base_assignment(scope, &write.body, &label)?;
    let symbol_scope = SymbolScope {
        constants: scope.constants,
        slots: &slots,
        overrides: &overrides,
    };
    let base = resolve_record_base(&expression, &symbol_scope)
        .map_err(|error| extract_error!("{label}: {error}"))?;
    let bases = base.bases(spec.count)?;
    let types = scope.index.types(scope.patterns, record_class)?;
    let context = FieldContext {
        scope,
        record_class,
        types: &types,
        symbols: scope.spec.record_symbols(&record_class.name),
        variable: &variable,
    };
    let mut fields = Vec::new();
    for (sequence, (writer, argument_text)) in direct_calls(scope.patterns, &write.body, &label)?
        .into_iter()
        .enumerate()
    {
        let sequence =
            u64::try_from(sequence).map_err(|_| extract_error!("record field count overflow"))?;
        fields.push(record_field(&context, sequence, &writer, &argument_text)?);
    }
    let expanded_fields = expand(spec.list, &record_class.name, &bases, &base.terms, &fields)?;
    let field_count_per_record = fields
        .iter()
        .filter(|field| field.role == Role::Field)
        .count();
    Ok(Record {
        name: spec.list.to_owned(),
        source_class: record_class.name.clone(),
        source_file: record_class.label.clone(),
        write_method: write.signature.clone(),
        write_method_line: u64::try_from(write.line)
            .map_err(|_| extract_error!("line overflow"))?,
        count: spec.count,
        offset_layout: layout_from(&base),
        record_base_offsets: bases,
        operation_count_per_record: u64::try_from(fields.len())
            .map_err(|_| extract_error!("field count overflow"))?,
        field_count_per_record: u64::try_from(field_count_per_record)
            .map_err(|_| extract_error!("field count overflow"))?,
        fields,
        expanded_fields,
    })
}

/// Verify that `expected` is the lowest address a fixed-base private writer
/// resolves, and return that address's dimension terms.
///
/// Candidates come from two places: assignments (`num = 880;`,
/// `num5 = 332812 + this.m_c;`) and the offset argument of every direct
/// write the codec classifier understands (`A_0.a((byte)this.m_a, 332810 +
/// this.m_c)`). Loop counters are literal assignments too (`num4 = 0;`), so
/// a literal-only assignment can confirm the pinned base but never lowers
/// it; an assignment takes part in the lowest-address check only when it
/// references a symbol.
fn verify_fixed_base(
    scope: &WriteScope<'_>,
    target: &ClassInfo,
    write: &MethodRef,
    slots: &[SlotSymbol],
    expected: u64,
) -> Result<Vec<Term>> {
    let assignment = regex::Regex::new(r"=\s*([^;=]+);")
        .map_err(|error| extract_error!("assignment pattern failed to compile: {error}"))?;
    let symbol_scope = SymbolScope {
        constants: scope.constants,
        slots,
        overrides: &[],
    };
    let label = format!("{}.{}", target.name, write.method);
    let mut pinned: Option<Address> = None;
    let mut lowest: Option<(Address, String)> = None;
    let mut consider = |expression: &str, symbolic: bool| {
        let Ok(address) = resolve_offset(expression, &symbol_scope) else {
            return;
        };
        if address.base == expected && pinned.is_none() {
            pinned = Some(address.clone());
        }
        if symbolic
            && lowest
                .as_ref()
                .is_none_or(|(low, _)| address.base < low.base)
        {
            lowest = Some((address, expression.to_owned()));
        }
    };
    for capture in assignment.captures_iter(&write.body) {
        let expression = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let symbolic = scope.patterns.identifier_re.is_match(expression);
        consider(expression, symbolic);
    }
    let types = scope.index.types(scope.patterns, target)?;
    for (method, arguments) in direct_calls(scope.patterns, &write.body, &label)? {
        let arguments = split_arguments(&arguments);
        if let Ok(classified) = classify_call(
            scope.patterns,
            &method,
            &arguments,
            &types,
            scope.constants,
            slots,
            scope.spec.value_helpers,
        ) {
            consider(&classified.offset_expression, true);
        }
    }
    let pinned = pinned
        .ok_or_else(|| extract_error!("{label} never resolves the pinned fixed base {expected}"))?;
    if let Some((low, expression)) = lowest
        && low.base < expected
    {
        return Err(extract_error!(
            "{label} writes at {} ({expression}), below the pinned fixed base {expected}",
            low.base
        ));
    }
    Ok(pinned.terms)
}

/// Catalog one private sub-writer, verifying the spec's pinned base.
pub(crate) fn catalog_private(
    scope: &WriteScope<'_>,
    owner: &Owner<'_>,
    spec: &PrivateWriterSpec,
    target: &ClassInfo,
    calls: &[&NestedCall],
) -> Result<PrivateRecord> {
    let call_count =
        u64::try_from(calls.len()).map_err(|_| extract_error!("call count overflow"))?;
    if call_count != spec.calls {
        return Err(extract_error!(
            "{} is called {call_count} times from {}, spec {} expects {}",
            target.name,
            owner.class.name,
            spec.name,
            spec.calls
        ));
    }
    let first = calls
        .first()
        .ok_or_else(|| extract_error!("private writer {} has no calls", spec.name))?;
    let (field, _) = first.split_target();
    let slots = inherited_slots(scope, owner, &field, target)?;
    let layout = if let Some(stride) = spec.stride {
        let write = child_writer(scope.patterns, target, &first.method, owner.writer_class)?;
        let label = format!("{}.{}", target.name, write.method);
        let (_, expression) = base_assignment(scope, &write.body, &label)?;
        let base = resolve_record_base(
            &expression,
            &SymbolScope {
                constants: scope.constants,
                slots: &slots,
                overrides: &[],
            },
        )
        .map_err(|error| extract_error!("{label}: {error}"))?;
        if base.base != spec.base || base.stride != stride {
            return Err(extract_error!(
                "{label} has base {} stride {}, spec {} pins {} and {stride}",
                base.base,
                base.stride,
                spec.name,
                spec.base
            ));
        }
        layout_from(&base)
    } else {
        let write = find_writer(owner.index, target, scope.patterns)?;
        if write.method != first.method {
            return Err(extract_error!(
                "{} calls {}.{} but that class's writer is {}",
                owner.class.name,
                target.name,
                first.method,
                write.method
            ));
        }
        let terms = verify_fixed_base(scope, target, &write, &slots, spec.base)?;
        OffsetLayout {
            kind: "fixed".to_owned(),
            base: spec.base,
            stride: None,
            overrides: Vec::new(),
            terms,
        }
    };
    Ok(PrivateRecord {
        name: spec.name.to_owned(),
        source_class: target.name.clone(),
        call_count,
        count: spec.count,
        offset_layout: layout,
        unsupported_public_reason: spec.reason.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::csharp::Patterns;
    use crate::model::THD75;
    use crate::sources::Sources;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    const OWNER: &str = "public class m1\n{\n\tprivate List<MyPositionData> u;\n\tpublic List<MyPositionData> MyPositionList\n\t{\n\t\tget\n\t\t{\n\t\t\treturn u;\n\t\t}\n\t}\n\tpublic void a0(m6 A_0)\n\t{\n\t\tu[num3].ax(A_0, num3);\n\t}\n}\n";
    const POSITION: &str = "public class MyPositionData\n{\n\tpublic int Altitude { get { return e; } }\n\tpublic byte MyPositionChannel { get { return f; } }\n\tpublic override void ax(m6 A_0, int A_1)\n\t{\n\t\tint num = 4384 + 32 * A_1;\n\t\tA_0.a(base.c, num + 12);\n\t\tA_0.b(e, 4, num);\n\t\tA_0.a(base.g, 2, num + 12);\n\t\tA_0.a(j, num + 4);\n\t\tA_0.a(m, num + 5);\n\t\tA_0.b(p, 2, num + 6);\n\t\tA_0.a(s, 3, num + 12);\n\t\tA_0.a(v, num + 8);\n\t\tA_0.a(y, num + 9);\n\t\tA_0.b(ab, 2, num + 10);\n\t\tA_0.a(f, num + 13);\n\t\tA_0.c(base.e, num + 14, nb.aa);\n\t}\n}\n";
    const CONSTANTS: &str =
        "public class nb\n{\n\tpublic static int aa;\n\tstatic nb()\n\t{\n\t\taa = 8;\n\t}\n}\n";

    fn scope<'a>(
        patterns: &'a Patterns,
        index: &'a ClassIndex,
        constants: &'a HashMap<String, i64>,
    ) -> WriteScope<'a> {
        WriteScope {
            patterns,
            spec: &THD75,
            index,
            constants,
            slots: &[],
            overrides: &[],
        }
    }

    #[test]
    fn coordinate_record_keeps_encoded_storage_transform() -> TestResult {
        let patterns = Patterns::new()?;
        let sources: Sources = vec![
            (PathBuf::from("m1.cs"), OWNER.to_owned()),
            (
                PathBuf::from("MCP.Models.MemoryMap/MyPositionData.cs"),
                POSITION.to_owned(),
            ),
            (PathBuf::from("nb.cs"), CONSTANTS.to_owned()),
        ];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let constants = crate::sources::parse_constants(&patterns, &sources);
        let scope = scope(&patterns, &index, &constants);
        let owner_class = index.get("m1").ok_or("m1 missing")?;
        let owner = Owner {
            index: &index,
            class: owner_class,
            slots: &[],
            writer_class: "m6",
        };
        let call = NestedCall {
            target: "u[num3]".to_owned(),
            method: "ax".to_owned(),
            index_expression: Some("num3".to_owned()),
        };
        let spec = THD75.records.first().ok_or("no record specs")?;
        let record = extract_record(&scope, &owner, spec, &call)?;
        assert_eq!(record.operation_count_per_record, 12);
        assert_eq!(record.field_count_per_record, 11);
        assert_eq!(
            record.record_base_offsets,
            vec![4384, 4416, 4448, 4480, 4512]
        );
        assert_eq!(record.expanded_fields.len(), 55);
        let encoded = record
            .expanded_fields
            .iter()
            .find(|field| field.name == "MyPositionList[0].LatitudeSecondEncoded")
            .ok_or("encoded latitude field missing")?;
        assert!(
            matches!(encoded.codec, Codec::UnsignedLe { width: 2, .. }),
            "{encoded:?}"
        );
        assert_eq!(
            encoded
                .storage_transform
                .as_ref()
                .map(|transform| transform.numerator),
            Some(10000)
        );
        let channel = record
            .expanded_fields
            .iter()
            .find(|field| field.name == "MyPositionList[4].MyPositionChannel")
            .ok_or("channel field missing")?;
        assert_eq!(channel.writable, Some(false));
        assert_eq!(channel.offset, 4512 + 13);
        Ok(())
    }

    #[test]
    fn catalogs_private_pair_and_blob() -> TestResult {
        let patterns = Patterns::new()?;
        let radio = "public class m9\n{\n\tprivate class a4\n\t{\n\t\tprivate int m_a;\n\t\tpublic void b(m6 A_0, int A_1)\n\t\t{\n\t\t\tint num3 = 848 + 16 * A_1;\n\t\t\tA_0.b(this.m_a, 2, num3);\n\t\t}\n\t}\n\tprivate class a5\n\t{\n\t\tprivate byte[] m_b = new byte[42];\n\t\tpublic void b(m6 A_0)\n\t\t{\n\t\t\tint num2 = default(int);\n\t\t\tnum2 = 880;\n\t\t\tA_0.a(this.m_b, num2);\n\t\t}\n\t}\n\tprivate a4 m_a = new a4();\n\tprivate a4 m_b = new a4();\n\tprivate a5 m_c = new a5();\n\tpublic void a0(m6 A_0)\n\t{\n\t\tthis.m_a.b(A_0, 0);\n\t\tthis.m_b.b(A_0, 1);\n\t\tthis.m_c.b(A_0);\n\t}\n}\n";
        let sources: Sources = vec![(PathBuf::from("m9.cs"), radio.to_owned())];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let constants = HashMap::new();
        let scope = scope(&patterns, &index, &constants);
        let owner_class = index.get("m9").ok_or("m9 missing")?;
        let owner = Owner {
            index: &index,
            class: owner_class,
            slots: &[],
            writer_class: "m6",
        };
        let pair_calls = [
            NestedCall {
                target: "this.m_a".to_owned(),
                method: "b".to_owned(),
                index_expression: Some("0".to_owned()),
            },
            NestedCall {
                target: "this.m_b".to_owned(),
                method: "b".to_owned(),
                index_expression: Some("1".to_owned()),
            },
        ];
        let pair_refs: Vec<&NestedCall> = pair_calls.iter().collect();
        let pair_spec = THD75.private_writers.first().ok_or("no private specs")?;
        let pair_class = index.get("m9.a4").ok_or("m9.a4 missing")?;
        let pair = catalog_private(&scope, &owner, pair_spec, pair_class, &pair_refs)?;
        assert_eq!(pair.name, "private_pair_848");
        assert_eq!(pair.offset_layout.kind, "linear");
        assert_eq!(pair.offset_layout.stride, Some(16));
        let blob_call = NestedCall {
            target: "this.m_c".to_owned(),
            method: "b".to_owned(),
            index_expression: None,
        };
        let blob_spec = THD75.private_writers.get(1).ok_or("no blob spec")?;
        let blob_class = index.get("m9.a5").ok_or("m9.a5 missing")?;
        let blob = catalog_private(&scope, &owner, blob_spec, blob_class, &[&blob_call])?;
        assert_eq!(blob.offset_layout.kind, "fixed");
        assert_eq!(blob.offset_layout.base, 880);
        Ok(())
    }
}
