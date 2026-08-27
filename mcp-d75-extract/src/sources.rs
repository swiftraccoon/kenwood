//! `ILSpy` project loading: source discovery, class types, constants.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::csharp::{Patterns, fancy_captures, normalize_whitespace, parse_integer};
use crate::error::{Result, extract_error};

/// Decompiled sources in sorted-path order.
pub(crate) type Sources = Vec<(PathBuf, String)>;

/// A path rendered with forward slashes.
pub(crate) fn posix_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The path label recorded in the manifest: relative to the source root when
/// possible, else the bare file name.
pub(crate) fn source_label(path: &Path, source_dir: &Path) -> String {
    path.strip_prefix(source_dir).map_or_else(
        |_| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        posix_path,
    )
}

fn collect_cs_files(directory: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| extract_error!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| extract_error!("cannot read {}: {error}", directory.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_cs_files(&path, found)?;
        } else if path.extension().is_some_and(|extension| extension == "cs") {
            found.push(path);
        }
    }
    Ok(())
}

/// Read every C# file under the `ILSpy` project directory.
///
/// A UTF-8 BOM is dropped and CRLF line endings become LF, so a project
/// decompiled on Windows (or a checkout that converted line endings)
/// parses and digests exactly like one written with LF. The line-oriented
/// patterns anchor on `$`, which never absorbs a trailing `\r`.
pub(crate) fn read_sources(source_dir: &Path) -> Result<Sources> {
    let mut paths = Vec::new();
    collect_cs_files(source_dir, &mut paths)?;
    paths.sort_by_key(|path| posix_path(path));
    let mut sources = Vec::new();
    for path in paths {
        let raw = std::fs::read(&path)
            .map_err(|error| extract_error!("cannot read {}: {error}", path.display()))?;
        let without_bom = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
        let text = String::from_utf8(without_bom.to_vec()).map_err(|error| {
            extract_error!("cannot decode {} as UTF-8: {error}", path.display())
        })?;
        sources.push((path, text.replace("\r\n", "\n")));
    }
    if sources.is_empty() {
        return Err(extract_error!(
            "no C# sources found under {}",
            source_dir.display()
        ));
    }
    Ok(sources)
}

fn first_class_name(patterns: &Patterns, source: &str) -> Option<String> {
    patterns
        .class_re
        .captures(source)
        .and_then(|capture| capture.get(1))
        .map(|name| name.as_str().to_owned())
}

/// Resolve decompiler-emitted static integer length constants (for example `nb.c`).
pub(crate) fn parse_constants(patterns: &Patterns, sources: &Sources) -> HashMap<String, i64> {
    let mut constants = HashMap::new();
    for (_path, source) in sources {
        let Some(class_name) = first_class_name(patterns, source) else {
            continue;
        };
        let declared: HashSet<&str> = patterns
            .static_int_decl_re
            .captures_iter(source)
            .filter_map(|capture| capture.get(1).map(|m| m.as_str()))
            .collect();
        for capture in patterns.constant_assign_re.captures_iter(source) {
            let name = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
            let value_text = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
            if declared.contains(name)
                && let Some(value) = parse_integer(patterns, value_text)
            {
                let _previous = constants.insert(format!("{class_name}.{name}"), value);
            }
        }
    }
    constants
}

/// Resolve a literal or a known static length constant to an integer.
pub(crate) fn resolve_integer(
    patterns: &Patterns,
    text: &str,
    constants: &HashMap<String, i64>,
) -> Option<i64> {
    parse_integer(patterns, text).or_else(|| constants.get(text.trim()).copied())
}

/// A class's public properties, private fields, and the enum types its
/// members may carry.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClassTypes {
    /// Public property name to normalized C# type.
    pub(crate) properties: HashMap<String, String>,
    /// Private field name to normalized C# type.
    pub(crate) private_fields: HashMap<String, String>,
    /// Enum type as written in this class (`ad`, `oa.ad`) to its qualified
    /// `<declaring class>.<enum>` name.
    pub(crate) enums: HashMap<String, String>,
}

/// Collect a class's public properties, private fields, and nested enums.
///
/// Nested enums are qualified with `class_name`; enums declared elsewhere
/// and referenced through a member type are resolved by
/// [`crate::class_index::ClassIndex::types`].
pub(crate) fn parse_types(
    patterns: &Patterns,
    class_name: &str,
    source: &str,
) -> Result<ClassTypes> {
    let mut types = ClassTypes::default();
    for capture in fancy_captures(&patterns.property_re, source)? {
        let kind = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let name = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
        drop(
            types
                .properties
                .insert(name.to_owned(), normalize_whitespace(kind)),
        );
    }
    for capture in patterns.field_re.captures_iter(source) {
        let kind = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
        let name = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
        drop(
            types
                .private_fields
                .insert(name.to_owned(), normalize_whitespace(kind)),
        );
    }
    for capture in patterns.enum_re.captures_iter(source) {
        if let Some(name) = capture.get(1) {
            drop(types.enums.insert(
                name.as_str().to_owned(),
                format!("{class_name}.{}", name.as_str()),
            ));
        }
    }
    Ok(types)
}
