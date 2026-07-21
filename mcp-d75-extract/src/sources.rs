//! `ILSpy` project loading: source discovery, class indexing, constants.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::csharp::{Patterns, fancy_captures, normalize_whitespace, parse_integer};
use crate::error::{Result, extract_error};
use crate::tables::SERIALIZERS;

/// Decompiled sources in sorted-path order, mirroring the Python dict.
pub(crate) type Sources = Vec<(PathBuf, String)>;

/// Serializer discovery result: `(menu, (class name, path, source))` in
/// serializer-roster order.
pub(crate) type Discovered = Vec<(&'static str, (String, PathBuf, String))>;

/// A path rendered with forward slashes, matching `PurePath.as_posix()`.
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
        sources.push((path, text));
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

/// Map each menu to its decompiled serializer class, path, and source.
pub(crate) fn discover_serializers(patterns: &Patterns, sources: &Sources) -> Result<Discovered> {
    let mut class_sources: HashMap<String, (PathBuf, String)> = HashMap::new();
    let mut public_to_class: HashMap<String, String> = HashMap::new();
    for (path, source) in sources {
        if let Some(class_name) = first_class_name(patterns, source) {
            drop(class_sources.insert(class_name, (path.clone(), source.clone())));
        }
        for capture in patterns.menu_property_re.captures_iter(source) {
            let (class_name, public_name) = (
                capture.get(1).map(|m| m.as_str()).unwrap_or_default(),
                capture.get(2).map(|m| m.as_str()).unwrap_or_default(),
            );
            drop(public_to_class.insert(public_name.to_owned(), class_name.to_owned()));
        }
    }

    let mut discovered = Vec::new();
    for (menu, public_name) in SERIALIZERS {
        let class_name = public_to_class.get(public_name).ok_or_else(|| {
            extract_error!(
                "cannot map {public_name} to its decompiled class; \
                 the source directory must include the MemoryMap container"
            )
        })?;
        let (path, source) = class_sources.get(class_name).ok_or_else(|| {
            extract_error!("source for class {class_name} ({public_name}) not found")
        })?;
        discovered.push((menu, (class_name.clone(), path.clone(), source.clone())));
    }
    Ok(discovered)
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

/// A class's public properties, private fields, and enum names.
#[derive(Debug, Default)]
pub(crate) struct ClassTypes {
    /// Public property name to normalized C# type.
    pub(crate) properties: HashMap<String, String>,
    /// Private field name to normalized C# type.
    pub(crate) private_fields: HashMap<String, String>,
    /// Nested enum names.
    pub(crate) enums: HashSet<String>,
}

/// Collect a class's public properties, private fields, and enum names.
pub(crate) fn parse_types(patterns: &Patterns, source: &str) -> Result<ClassTypes> {
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
            let _fresh = types.enums.insert(name.as_str().to_owned());
        }
    }
    Ok(types)
}

/// Index top-level decompiled classes by their metadata name.
pub(crate) fn class_source_index(
    patterns: &Patterns,
    sources: &Sources,
) -> Result<HashMap<String, (PathBuf, String)>> {
    let mut result: HashMap<String, (PathBuf, String)> = HashMap::new();
    for (path, source) in sources {
        let Some(name) = first_class_name(patterns, source) else {
            continue;
        };
        if let Some((existing, _)) = result.get(&name) {
            return Err(extract_error!(
                "duplicate source class {name}: {} and {}",
                existing.display(),
                path.display()
            ));
        }
        drop(result.insert(name, (path.clone(), source.clone())));
    }
    Ok(result)
}
