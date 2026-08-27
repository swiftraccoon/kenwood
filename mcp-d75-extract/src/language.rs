//! Kenwood's UTF-16 language INI and combo-box label joins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::csharp::{Patterns, normalize_whitespace, split_arguments};
use crate::error::{Result, extract_error};
use crate::manifest::LanguageFileInfo;
use crate::sources::Sources;

/// Language INI sections: `section -> key -> value`.
pub(crate) type LanguageSections = HashMap<String, HashMap<String, String>>;

/// Resource key and display label derived from a `DisplayMember` expression.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ComboMeta {
    /// Official language-resource key, when the label is resource-backed.
    pub(crate) resource_key: Option<String>,
    /// English display label, when one is derivable.
    pub(crate) label: Option<String>,
}

/// `(owner class, enum, member) -> metadata`.
pub(crate) type ComboOptions = HashMap<(String, String, String), ComboMeta>;

/// Per-run patterns built from the discovered resource singleton class.
struct ResourcePatterns {
    key: regex::Regex,
    format: regex::Regex,
    access_key_prefix: String,
}

impl ResourcePatterns {
    fn new(resource_class: &str) -> Result<Self> {
        let escaped = regex::escape(resource_class);
        Ok(Self {
            key: regex::Regex::new(&format!(r"\b{escaped}\.Instance\.([@\w]+)")).map_err(
                |error| extract_error!("resource key pattern failed to compile: {error}"),
            )?,
            format: regex::Regex::new(&format!(
                r"(?s)^string\.Format\(\s*{escaped}\.Instance\.([@\w]+)\s*,\s*(.*)\)$"
            ))
            .map_err(|error| extract_error!("format label pattern failed to compile: {error}"))?,
            access_key_prefix: format!("{resource_class}.a("),
        })
    }
}

fn decode_utf16(raw: &[u8], path: &Path) -> Result<String> {
    let invalid = || {
        extract_error!(
            "language file is not valid BOM-marked UTF-16: {}",
            path.display()
        )
    };
    let (payload, big_endian) = match raw {
        [0xFF, 0xFE, rest @ ..] => (rest, false),
        [0xFE, 0xFF, rest @ ..] => (rest, true),
        _ => (raw, false),
    };
    if payload.len() % 2 != 0 {
        return Err(invalid());
    }
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|pair| {
            let low = pair.first().copied().unwrap_or_default();
            let high = pair.get(1).copied().unwrap_or_default();
            if big_endian {
                u16::from_be_bytes([low, high])
            } else {
                u16::from_le_bytes([low, high])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|_| invalid())
}

/// Split text on `\r\n`, `\r`, and `\n` line boundaries.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes.get(index) {
            Some(b'\n') => {
                lines.push(text.get(start..index).unwrap_or_default());
                index += 1;
                start = index;
            }
            Some(b'\r') => {
                lines.push(text.get(start..index).unwrap_or_default());
                index += if bytes.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < bytes.len() {
        lines.push(text.get(start..).unwrap_or_default());
    }
    lines
}

/// Read Kenwood's UTF-16 language INI without configparser interpolation.
pub(crate) fn parse_language_file(
    path: Option<&PathBuf>,
) -> Result<(LanguageSections, Option<LanguageFileInfo>)> {
    let Some(path) = path else {
        return Ok((HashMap::new(), None));
    };
    let raw = std::fs::read(path)
        .map_err(|error| extract_error!("cannot read language file {}: {error}", path.display()))?;
    let text = decode_utf16(&raw, path)?;
    let mut sections: LanguageSections = HashMap::new();
    let mut current: Option<String> = None;
    for raw_line in split_lines(&text) {
        let stripped = raw_line.trim();
        if stripped.is_empty() || stripped.starts_with(';') || stripped.starts_with('#') {
            continue;
        }
        if stripped.starts_with('[') && stripped.ends_with(']') {
            let section = stripped
                .get(1..stripped.len() - 1)
                .unwrap_or_default()
                .trim()
                .to_owned();
            let _section = sections.entry(section.clone()).or_default();
            current = Some(section);
            continue;
        }
        if let Some(section) = current.as_ref()
            && let Some((key, value)) = raw_line.split_once('=')
        {
            drop(
                sections
                    .entry(section.clone())
                    .or_default()
                    .insert(key.trim().to_owned(), value.trim().to_owned()),
            );
        }
    }
    let info = LanguageFileInfo {
        file_name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sha256: format!("{:x}", Sha256::digest(&raw)),
        encoding: "UTF-16".to_owned(),
    };
    Ok((sections, Some(info)))
}

/// Resolve a resource property by its longest matching INI section prefix.
pub(crate) fn language_label(resource_key: &str, sections: &LanguageSections) -> Option<String> {
    let mut names: Vec<&String> = sections.keys().collect();
    names.sort_by(|left, right| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left.as_str().cmp(right.as_str()))
    });
    for section in names {
        if let Some(key) = resource_key.strip_prefix(section.as_str())
            && let Some(value) = sections.get(section).and_then(|entries| entries.get(key))
        {
            return Some(value.clone());
        }
    }
    None
}

/// Substitute C# `string.Format` arguments into a label template.
pub(crate) fn format_resource_label(
    patterns: &Patterns,
    template: &str,
    argument_text: &str,
) -> String {
    let arguments = split_arguments(argument_text);
    patterns
        .format_placeholder_re
        .replace_all(template, |captures: &regex::Captures<'_>| {
            let whole = captures
                .get(0)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_owned();
            let Some(index) = captures
                .get(1)
                .and_then(|digits| digits.as_str().parse::<usize>().ok())
            else {
                return whole;
            };
            let Some(argument) = arguments.get(index) else {
                return whole;
            };
            let argument = argument.trim();
            patterns.quoted_string_re.captures(argument).map_or_else(
                || argument.to_owned(),
                |quoted| {
                    quoted
                        .get(1)
                        .map(|m| m.as_str())
                        .unwrap_or_default()
                        .to_owned()
                },
            )
        })
        .into_owned()
}

fn combo_label_metadata(
    patterns: &Patterns,
    resources: &ResourcePatterns,
    expression: &str,
    sections: &LanguageSections,
) -> ComboMeta {
    let normalized = normalize_whitespace(expression);
    let mut metadata = ComboMeta::default();
    if let Some(resource) = resources.key.captures(&normalized) {
        let resource_key = resource
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_default()
            .to_owned();
        let label = language_label(&resource_key, sections);
        metadata.resource_key = Some(resource_key);
        if let Some(label) = label {
            let formatted = resources.format.captures(&normalized).map_or_else(
                || {
                    if normalized.starts_with(&resources.access_key_prefix) {
                        // The access-key helper strips the mnemonic underscore before display.
                        label.replace('_', "")
                    } else {
                        label.clone()
                    }
                },
                |format| {
                    format_resource_label(
                        patterns,
                        &label,
                        format.get(2).map(|m| m.as_str()).unwrap_or_default(),
                    )
                },
            );
            metadata.label = Some(formatted);
        }
        return metadata;
    }
    if let Some(number) = patterns.number_label_re.captures(&normalized) {
        metadata.label = number.get(1).map(|m| m.as_str().to_owned());
    }
    metadata
}

/// Join enum members to the combo-box display expressions of the program.
pub(crate) fn parse_combo_options(
    patterns: &Patterns,
    sources: &Sources,
    sections: &LanguageSections,
    classes: &[String],
    resource_class: &str,
) -> Result<ComboOptions> {
    let resources = ResourcePatterns::new(resource_class)?;
    let mut unique: Vec<&String> = classes.iter().collect();
    unique.sort();
    unique.dedup();
    let class_pattern = unique
        .iter()
        .map(|name| regex::escape(name))
        .collect::<Vec<_>>()
        .join("|");
    let entry_pattern = regex::Regex::new(&format!(
        r"(?s)Value\s*=\s*({class_pattern})\.([@\w]+)\.([@\w]+)\s*,\s*DisplayMember\s*=\s*(.*?)\s*\n\s*\}}"
    ))
    .map_err(|error| extract_error!("combo entry pattern failed to compile: {error}"))?;
    let mut options: ComboOptions = HashMap::new();
    for (path, source) in sources {
        for capture in entry_pattern.captures_iter(source) {
            let part = |index: usize| -> String {
                capture
                    .get(index)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let key = (part(1), part(2), part(3));
            let metadata = combo_label_metadata(
                patterns,
                &resources,
                capture.get(4).map(|m| m.as_str()).unwrap_or_default(),
                sections,
            );
            if let Some(existing) = options.get(&key) {
                if *existing != metadata {
                    let file_name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    return Err(extract_error!(
                        "conflicting combo labels for {}.{}.{} in {file_name}: {existing:?} versus {metadata:?}",
                        key.0,
                        key.1,
                        key.2
                    ));
                }
            } else {
                drop(options.insert(key, metadata));
            }
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn combo_join_uses_the_discovered_resource_class() -> TestResult {
        let patterns = Patterns::new()?;
        let source = "new gd\n{\n\tValue = oa.a.a,\n\tDisplayMember = ky.Instance.Edit_Menu_TestBeatShift_Off\n}\nnew gd\n{\n\tValue = oa.a.b,\n\tDisplayMember = string.Format(ky.Instance.Edit_Menu_TestBeatShift_Numbered, 1)\n}\n";
        let sources: Sources = vec![(PathBuf::from("combo.cs"), source.to_owned())];
        let mut sections: LanguageSections = HashMap::new();
        drop(sections.insert(
            "Edit_Menu_Test".to_owned(),
            HashMap::from([
                ("BeatShift_Off".to_owned(), "Off".to_owned()),
                ("BeatShift_Numbered".to_owned(), "Choice {0}".to_owned()),
            ]),
        ));
        let combos = parse_combo_options(&patterns, &sources, &sections, &["oa".to_owned()], "ky")?;
        let off = combos
            .get(&("oa".to_owned(), "a".to_owned(), "a".to_owned()))
            .ok_or("member a missing")?;
        assert_eq!(off.label.as_deref(), Some("Off"));
        let numbered = combos
            .get(&("oa".to_owned(), "a".to_owned(), "b".to_owned()))
            .ok_or("member b missing")?;
        assert_eq!(numbered.label.as_deref(), Some("Choice 1"));
        let other_class =
            parse_combo_options(&patterns, &sources, &sections, &["oa".to_owned()], "kb")?;
        assert_eq!(
            other_class.len(),
            2,
            "entries are keyed by class, not resource"
        );
        assert!(
            other_class
                .values()
                .all(|meta| meta.resource_key.is_none() && meta.label.is_none()),
            "a wrong resource class must yield no keys or labels: {other_class:?}"
        );
        Ok(())
    }
}
