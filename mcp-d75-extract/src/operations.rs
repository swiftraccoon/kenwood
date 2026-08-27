//! Direct writes of one writer body, resolved to addresses.

use std::collections::HashMap;

use crate::address::{Address, SlotSymbol, SymbolScope, resolve_offset};
use crate::class_index::{ClassIndex, ClassInfo};
use crate::codecs::classify_call;
use crate::csharp::{Patterns, split_arguments};
use crate::discovery::MethodRef;
use crate::error::{Result, extract_error};
use crate::manifest::{Codec, Operation, Role, offset_hex};
use crate::model::ModelSpec;

/// Everything a writer body's offsets and values may reference.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteScope<'a> {
    /// Compiled patterns.
    pub(crate) patterns: &'a Patterns,
    /// Model spec (domains, blobs, helpers).
    pub(crate) spec: &'static ModelSpec,
    /// Project class index (member types, enum declaring classes).
    pub(crate) index: &'a ClassIndex,
    /// Project static constants.
    pub(crate) constants: &'a HashMap<String, i64>,
    /// Slot symbols of the class being read.
    pub(crate) slots: &'a [SlotSymbol],
    /// Pinned base-override symbols.
    pub(crate) overrides: &'a [(String, u64)],
}

impl WriteScope<'_> {
    /// The symbol scope for offset resolution.
    pub(crate) const fn symbols(&self) -> SymbolScope<'_> {
        SymbolScope {
            constants: self.constants,
            slots: self.slots,
            overrides: self.overrides,
        }
    }
}

/// Every one-line `A_0.<method>(<args>);` statement, checked against the
/// number of `A_0.` mentions so no write hides in another shape.
pub(crate) fn direct_calls(
    patterns: &Patterns,
    body: &str,
    label: &str,
) -> Result<Vec<(String, String)>> {
    let mentions = patterns.a0_mention_re.find_iter(body).count();
    let calls: Vec<(String, String)> = patterns
        .direct_call_re
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
    if calls.len() != mentions {
        return Err(extract_error!(
            "{label} has {mentions} A_0 mentions but only {} match the supported one-line call shape",
            calls.len()
        ));
    }
    Ok(calls)
}

fn apply_blob_spec(scope: &WriteScope<'_>, operation: &mut Operation) {
    let Some(name) = operation.name.as_deref() else {
        return;
    };
    let Some(blob) = scope.spec.blob(name) else {
        return;
    };
    operation.category = Some("blob".to_owned());
    if !blob.writable {
        operation.writable = Some(false);
        operation.not_writable_reason = blob.reason.map(ToOwned::to_owned);
    }
}

/// Give raw byte writes the length of a clear at the same address.
fn infer_blob_lengths(operations: &mut [Operation]) {
    let clears: Vec<(Address, u64)> = operations
        .iter()
        .filter_map(|operation| match &operation.codec {
            Codec::ClearRange { length, .. } if operation.role == Role::Clear => {
                Some((operation.address.clone(), *length))
            }
            _ => None,
        })
        .collect();
    for operation in operations.iter_mut() {
        let address = operation.address.clone();
        if let Codec::RawBytes { length, .. } = &mut operation.codec
            && length.is_none()
            && let Some((_, cleared)) = clears.iter().find(|(clear, _)| *clear == address)
        {
            *length = Some(*cleared);
        }
    }
}

/// Extract the direct writes of `write` in `class`, numbering from `first_sequence`.
pub(crate) fn extract_operations(
    scope: &WriteScope<'_>,
    class: &ClassInfo,
    write: &MethodRef,
    menu_key: &str,
    first_sequence: u64,
) -> Result<Vec<Operation>> {
    let types = scope.index.types(scope.patterns, class)?;
    let label = format!("{}.{}", class.name, write.method);
    let mut operations = Vec::new();
    for (position, (method, argument_text)) in direct_calls(scope.patterns, &write.body, &label)?
        .into_iter()
        .enumerate()
    {
        let args = split_arguments(&argument_text);
        let classified = classify_call(
            scope.patterns,
            &method,
            &args,
            &types,
            scope.constants,
            scope.slots,
            scope.spec.value_helpers,
        )
        .map_err(|error| extract_error!("{label}: {error}"))?;
        if classified.codec.value_type() == Some("unknown") {
            return Err(extract_error!(
                "{label}: the value of A_0.{method}({}) is neither a typed member of {} nor a spec'd symbol",
                args.join(", "),
                class.name
            ));
        }
        let address = resolve_offset(&classified.offset_expression, &scope.symbols())
            .map_err(|error| extract_error!("{label}: {error}"))?;
        let sequence = first_sequence
            + u64::try_from(position).map_err(|_| extract_error!("operation count overflow"))?;
        let domain = classified
            .name
            .as_deref()
            .filter(|_| classified.role == Role::Field)
            .and_then(|name| scope.spec.direct_domain(&format!("{menu_key}.{name}")));
        let mut operation = Operation {
            sequence,
            role: classified.role,
            name: classified.name,
            writer_class: class.name.clone(),
            offset: address.base,
            offset_hex: offset_hex(address.base),
            address,
            codec: classified.codec,
            domain,
            category: None,
            writable: None,
            not_writable_reason: None,
        };
        apply_blob_spec(scope, &mut operation);
        operations.push(operation);
    }
    infer_blob_lengths(&mut operations);
    Ok(operations)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::class_index::ClassIndex;
    use crate::discovery::find_writer;
    use crate::model::THD75;
    use crate::sources::Sources;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    const RADIO: &str = "public class m9\n{\n\tpublic enum a : byte\n\t{\n\t\ta,\n\t\tb = 4,\n\t}\n\tprivate byte[] m_a0;\n\tpublic a BeatShift\n\t{\n\t\tget { return a.a; }\n\t}\n\tpublic byte TxEqLevel04\n\t{\n\t\tget { return 0; }\n\t}\n\tpublic byte[] PoweronBitmap\n\t{\n\t\tget { return m_a0; }\n\t}\n\tpublic byte[] GpsLogBitmap\n\t{\n\t\tget { return m_a0; }\n\t}\n\tpublic void a0(m6 A_0)\n\t{\n\t\tA_0.a(327680, 86400);\n\t\tA_0.a(GpsLogBitmap, 414080);\n\t\tA_0.a(414080, 86400);\n\t\tA_0.a(PoweronBitmap, 327680);\n\t\tA_0.a((byte)BeatShift, 4096);\n\t\tA_0.a(TxEqLevel04, 4200);\n\t}\n}\n";

    #[test]
    fn extracts_addresses_domains_and_blobs() -> TestResult {
        let patterns = Patterns::new()?;
        let sources: Sources = vec![(PathBuf::from("m9.cs"), RADIO.to_owned())];
        let index = ClassIndex::build(&patterns, &sources, Path::new(""))?;
        let class = index.get("m9").ok_or("m9 missing")?;
        let write = find_writer(&index, class, &patterns)?;
        let constants = HashMap::new();
        let scope = WriteScope {
            patterns: &patterns,
            spec: &THD75,
            index: &index,
            constants: &constants,
            slots: &[],
            overrides: &[],
        };
        let operations = extract_operations(&scope, class, &write, "radio", 0)?;
        assert_eq!(operations.len(), 6);
        let gps_log = operations.get(1).ok_or("no second operation")?;
        assert_eq!(gps_log.name.as_deref(), Some("GpsLogBitmap"));
        assert_eq!(gps_log.offset_hex, "0x65180");
        assert!(
            matches!(
                gps_log.codec,
                Codec::RawBytes {
                    length: Some(86_400),
                    ..
                }
            ),
            "{gps_log:?}"
        );
        assert_eq!(gps_log.category.as_deref(), Some("blob"));
        assert_eq!(gps_log.writable, Some(false));
        let poweron = operations.get(3).ok_or("no fourth operation")?;
        assert_eq!(poweron.writable, None);
        assert!(
            matches!(
                poweron.codec,
                Codec::RawBytes {
                    length: Some(86_400),
                    ..
                }
            ),
            "{poweron:?}"
        );
        let eq = operations.get(5).ok_or("no sixth operation")?;
        assert!(
            eq.domain.is_some(),
            "TxEqLevel04 must carry its audited domain"
        );
        assert!(eq.address.is_absolute());
        assert_eq!(eq.writer_class, "m9");
        Ok(())
    }
}
