//! Enum definitions and the per-menu enum catalogs.

use std::collections::{BTreeMap, BTreeSet};

use crate::class_index::ClassIndex;
use crate::csharp::Patterns;
use crate::error::{Result, extract_error};
use crate::language::ComboOptions;
use crate::manifest::{EnumCatalog, EnumOption, Menu, RecordEntry, Role};

/// One nested enum declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnumDefinition {
    /// Decompiled enum name.
    pub(crate) csharp_name: String,
    /// Underlying integer type (`int` when unspecified).
    pub(crate) underlying_type: String,
    /// Members with C#'s implicit increments applied.
    pub(crate) options: Vec<EnumOption>,
}

/// Parse every `public enum` of one class body.
pub(crate) fn parse_enum_definitions(
    patterns: &Patterns,
    source: &str,
) -> Result<BTreeMap<String, EnumDefinition>> {
    let mut definitions = BTreeMap::new();
    for capture in patterns.enum_body_re.captures_iter(source) {
        let name = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let underlying_type = capture.get(2).map_or("int", |m| m.as_str());
        let raw_body = capture.get(3).map(|m| m.as_str()).unwrap_or_default();
        let body = patterns.enum_comment_re.replace_all(raw_body, "");
        let mut next_value: i64 = 0;
        let mut options = Vec::new();
        for raw_entry in body.split(',') {
            let entry = raw_entry.trim();
            if entry.is_empty() {
                continue;
            }
            let entry_capture = patterns
                .enum_member_re
                .captures(entry)
                .ok_or_else(|| extract_error!("unsupported enum member in {name}: {entry:?}"))?;
            let member = entry_capture.get(1).map(|m| m.as_str()).unwrap_or_default();
            if let Some(explicit) = entry_capture.get(2)
                && let Some(value) = crate::csharp::parse_integer(patterns, explicit.as_str())
            {
                next_value = value;
            }
            options.push(EnumOption {
                value: next_value,
                member: member.to_owned(),
                resource_key: None,
                label: None,
            });
            next_value += 1;
        }
        drop(definitions.insert(
            name.to_owned(),
            EnumDefinition {
                csharp_name: name.to_owned(),
                underlying_type: underlying_type.to_owned(),
                options,
            },
        ));
    }
    Ok(definitions)
}

/// `(declaring class, enum name)` pairs of the enum-valued field codecs of a
/// menu, from the qualified names the classifier resolved.
fn used_enum_types(menu: &Menu) -> Result<BTreeSet<(String, String)>> {
    let mut used = BTreeSet::new();
    let mut note = |qualified: &str| -> Result<()> {
        let (owner, enum_name) = qualified
            .rsplit_once('.')
            .ok_or_else(|| extract_error!("enum type {qualified} is not class-qualified"))?;
        let _fresh = used.insert((owner.to_owned(), enum_name.to_owned()));
        Ok(())
    };
    for operation in &menu.operations {
        if operation.role == Role::Field
            && let Some(qualified) = operation.codec.enum_type()
        {
            note(qualified)?;
        }
    }
    for entry in &menu.repeated_records {
        let RecordEntry::Extracted(record) = entry else {
            continue;
        };
        for field in &record.fields {
            if field.role == Role::Field
                && let Some(qualified) = field.codec.enum_type()
            {
                note(qualified)?;
            }
        }
    }
    Ok(used)
}

/// Attach each menu's enum catalogs.
///
/// Enum-valued codecs already carry their qualified `enum_type`; this pass
/// reads the definitions from the declaring classes and joins the combo-box
/// labels.
pub(crate) fn attach_enum_catalogs(
    patterns: &Patterns,
    index: &ClassIndex,
    menus: &mut [Menu],
    combos: &ComboOptions,
) -> Result<()> {
    for menu in menus.iter_mut() {
        let used = used_enum_types(menu)?;
        let mut catalogs = Vec::new();
        for (owner, enum_name) in &used {
            let class = index
                .get(owner)
                .ok_or_else(|| extract_error!("source for enum owner {owner} not found"))?;
            let definitions = parse_enum_definitions(patterns, &class.own_text)?;
            let definition = definitions.get(enum_name).ok_or_else(|| {
                extract_error!(
                    "enum definition {owner}.{enum_name} used by {} was not found",
                    menu.public_name
                )
            })?;
            let qualified = format!("{owner}.{enum_name}");
            let mut options = definition.options.clone();
            for option in &mut options {
                if let Some(meta) =
                    combos.get(&(owner.clone(), enum_name.clone(), option.member.clone()))
                {
                    option.resource_key.clone_from(&meta.resource_key);
                    option.label.clone_from(&meta.label);
                }
            }
            catalogs.push(EnumCatalog {
                name: qualified,
                csharp_name: definition.csharp_name.clone(),
                underlying_type: definition.underlying_type.clone(),
                options,
            });
        }
        menu.enum_types = catalogs;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn parses_implicit_and_explicit_members() -> TestResult {
        let patterns = Patterns::new()?;
        let source = "public class m9\n{\n\tpublic enum a : byte\n\t{\n\t\ta,\n\t\tb = 4,\n\t\tc, // trailing comment\n\t}\n\tpublic enum ad\n\t{\n\t\tx,\n\t}\n}\n";
        let definitions = parse_enum_definitions(&patterns, source)?;
        let a = definitions.get("a").ok_or("enum a missing")?;
        assert_eq!(a.underlying_type, "byte");
        let members: Vec<(i64, &str)> = a
            .options
            .iter()
            .map(|option| (option.value, option.member.as_str()))
            .collect();
        assert_eq!(members, vec![(0, "a"), (4, "b"), (5, "c")]);
        assert_eq!(
            definitions
                .get("ad")
                .map(|definition| definition.underlying_type.as_str()),
            Some("int")
        );
        Ok(())
    }
}
