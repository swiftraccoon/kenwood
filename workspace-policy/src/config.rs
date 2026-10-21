//! Strict parsing of the declarative workspace inventory.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use toml::{Table, Value};

use crate::PolicyResult;

/// A Cargo lint namespace supported by the workspace policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LintGroup {
    Rust,
    Clippy,
    Rustdoc,
}

impl LintGroup {
    /// Reject an unknown namespace before constructing a lint key.
    pub(crate) fn parse(value: &str) -> PolicyResult<Self> {
        match value {
            "rust" => Ok(Self::Rust),
            "clippy" => Ok(Self::Clippy),
            "rustdoc" => Ok(Self::Rustdoc),
            _ => Err(format!("unknown lint group {value}").into()),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Clippy => "clippy",
            Self::Rustdoc => "rustdoc",
        }
    }
}

impl fmt::Display for LintGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub(crate) struct Package {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct Deviation {
    pub(crate) package: String,
    pub(crate) group: LintGroup,
    pub(crate) lint: String,
    pub(crate) level: String,
}

#[derive(Debug)]
pub(crate) struct Policy {
    pub(crate) packages: Vec<Package>,
    pub(crate) fuzz: BTreeSet<PathBuf>,
    pub(crate) deviations: Vec<Deviation>,
}

impl Policy {
    pub(crate) fn read(root: &Path) -> PolicyResult<Self> {
        let source = fs::read_to_string(root.join(".config/rust-policy.toml"))?;
        let document = source.parse::<Table>()?;
        keys(&document, &["version", "packages", "fuzz", "deviations"])?;
        if document.get("version").and_then(Value::as_integer) != Some(1) {
            return Err("rust policy needs version = 1".into());
        }
        let packages = records(&document, "packages")?
            .iter()
            .map(parse_package)
            .collect::<PolicyResult<Vec<_>>>()?;
        if packages.is_empty() {
            return Err("rust policy needs at least one package".into());
        }
        let mut fuzz = BTreeSet::new();
        for record in records(&document, "fuzz")? {
            let record = record.as_table().ok_or("fuzz record must be a table")?;
            keys(record, &["manifest"])?;
            let manifest = relative_path(string(record, "manifest")?)?;
            if !fuzz.insert(manifest) {
                return Err("duplicate fuzz manifest in rust policy".into());
            }
        }
        let deviations = records(&document, "deviations")?
            .iter()
            .map(parse_deviation)
            .collect::<PolicyResult<Vec<_>>>()?;
        Ok(Self {
            packages,
            fuzz,
            deviations,
        })
    }
}

fn parse_package(value: &Value) -> PolicyResult<Package> {
    let record = value.as_table().ok_or("package record must be a table")?;
    keys(record, &["name", "path"])?;
    Ok(Package {
        name: string(record, "name")?.to_owned(),
        path: relative_path(string(record, "path")?)?,
    })
}

fn parse_deviation(value: &Value) -> PolicyResult<Deviation> {
    let record = value.as_table().ok_or("deviation record must be a table")?;
    keys(record, &["package", "group", "lint", "level", "reason"])?;
    let _reason = string(record, "reason")?;
    let group = LintGroup::parse(string(record, "group")?)?;
    let level = string(record, "level")?;
    if level != "omitted" {
        let (name, priority) = level
            .split_once(':')
            .map_or((level, None), |(name, priority)| (name, Some(priority)));
        if !["allow", "warn", "force-warn", "deny", "forbid"].contains(&name) {
            return Err(format!("unknown lint level {level}").into());
        }
        if let Some(priority) = priority {
            let _priority = priority.parse::<i64>()?;
        }
    }
    Ok(Deviation {
        package: string(record, "package")?.to_owned(),
        group,
        lint: string(record, "lint")?.to_owned(),
        level: level.to_owned(),
    })
}

pub(crate) fn relative_path(value: &str) -> PolicyResult<PathBuf> {
    let path = PathBuf::from(value);
    if value.is_empty()
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '_' | '-' | '.')
        })
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(format!("policy path must be a portable relative path: {value:?}").into());
    }
    Ok(path)
}

pub(crate) fn string<'a>(record: &'a Table, key: &str) -> PolicyResult<&'a str> {
    let value = record
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {key}"))?;
    if value.trim().is_empty() {
        return Err(format!("empty string {key}").into());
    }
    Ok(value)
}

pub(crate) fn keys(record: &Table, allowed: &[&str]) -> PolicyResult<()> {
    for key in record.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown policy field {key}").into());
        }
    }
    Ok(())
}

fn records<'a>(record: &'a Table, key: &str) -> PolicyResult<&'a [Value]> {
    match record.get(key) {
        None => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        Some(_) => Err(format!("{key} must be an array of tables").into()),
    }
}
