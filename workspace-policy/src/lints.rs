//! Exact comparisons of inherited and restated Cargo lint policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use toml::{Table, Value};

use crate::PolicyResult;
use crate::config::{self, LintGroup, Policy};

type LintKey = (LintGroup, String);
type DeviationKey = (String, LintGroup, String);

pub(crate) fn check(root: &Path, defaults: &Table, policy: &Policy) -> PolicyResult<()> {
    let expected = flatten(defaults)?;
    check_required_root_lints(&expected)?;
    let mut deviations = BTreeMap::new();
    for deviation in &policy.deviations {
        let key = (
            deviation.package.clone(),
            deviation.group,
            deviation.lint.clone(),
        );
        if deviations.insert(key, &deviation.level).is_some() {
            return Err("duplicate lint deviation in rust policy".into());
        }
    }
    let mut used = BTreeSet::new();
    for package in &policy.packages {
        let source = fs::read_to_string(root.join(&package.path).join("Cargo.toml"))?;
        let manifest = source.parse::<Table>()?;
        let lints = manifest
            .get("lints")
            .and_then(Value::as_table)
            .ok_or_else(|| format!("{} needs inherited or restated lint tables", package.name))?;
        let actual = effective(lints, &expected, &source, &package.name)?;
        check_required_member_lints(&actual, &package.name)?;
        let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
        for key in keys {
            if expected.get(key) == actual.get(key) {
                continue;
            }
            let level = actual.get(key).map_or("omitted", String::as_str);
            let deviation_key = (package.name.clone(), key.0, key.1.clone());
            if deviations.get(&deviation_key).map(|entry| entry.as_str()) != Some(level) {
                return Err(format!("{} {}::{} differs from workspace policy: effective {level}; document the exact deviation", package.name, key.0, key.1).into());
            }
            let _inserted = used.insert(deviation_key);
        }
    }
    check_fuzz(root, &expected, policy)?;
    reject_unused(&deviations, &used)
}

/// Standalone workspaces restate the root policy and inherit it without exceptions.
fn check_fuzz(
    root: &Path,
    expected: &BTreeMap<LintKey, String>,
    policy: &Policy,
) -> PolicyResult<()> {
    for path in &policy.fuzz {
        let manifest = fs::read_to_string(root.join(path))?.parse::<Table>()?;
        let defaults = manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("lints"))
            .and_then(Value::as_table)
            .ok_or_else(|| format!("{} needs standalone workspace lint tables", path.display()))?;
        let actual = flatten(defaults)?;
        for key in expected.keys().chain(actual.keys()) {
            if actual.get(key) != expected.get(key) {
                return Err(format!(
                    "{} {}::{} differs from workspace policy: effective {}",
                    path.display(),
                    key.0,
                    key.1,
                    actual.get(key).map_or("omitted", String::as_str),
                )
                .into());
            }
        }
        let lints = manifest
            .get("lints")
            .and_then(Value::as_table)
            .ok_or_else(|| format!("{} needs inherited lint tables", path.display()))?;
        config::keys(lints, &["workspace"])?;
        if lints.get("workspace").and_then(Value::as_bool) != Some(true) {
            return Err(format!("{} must inherit its workspace lints", path.display()).into());
        }
    }
    Ok(())
}

fn check_required_root_lints(defaults: &BTreeMap<LintKey, String>) -> PolicyResult<()> {
    for (group, lint, required) in [
        (LintGroup::Rust, "unsafe_code", "forbid"),
        (LintGroup::Clippy, "all", "deny"),
        (LintGroup::Clippy, "pedantic", "deny"),
        (LintGroup::Clippy, "nursery", "deny"),
        (LintGroup::Clippy, "cargo", "deny"),
    ] {
        let setting = defaults.get(&(group, lint.to_owned()));
        let level = setting.map(String::as_str).map(lint_level);
        if level != Some(required) {
            return Err(format!("workspace root must set {group}::{lint} to {required}").into());
        }
    }
    Ok(())
}

fn check_required_member_lints(
    actual: &BTreeMap<LintKey, String>,
    package: &str,
) -> PolicyResult<()> {
    for lint in ["all", "pedantic", "nursery", "cargo"] {
        let level = actual
            .get(&(LintGroup::Clippy, lint.to_owned()))
            .map(String::as_str)
            .map(lint_level);
        if !matches!(level, Some("deny" | "forbid")) {
            return Err(format!(
                "{package} must set clippy::{lint} to deny or forbid; \
                 documented deviations cannot weaken a mandatory group (effective {})",
                level.unwrap_or("omitted")
            )
            .into());
        }
    }
    Ok(())
}

fn lint_level(setting: &str) -> &str {
    setting
        .split_once(':')
        .map_or(setting, |(level, _priority)| level)
}

fn reject_unused(
    deviations: &BTreeMap<DeviationKey, &String>,
    used: &BTreeSet<DeviationKey>,
) -> PolicyResult<()> {
    for key in deviations.keys() {
        if !used.contains(key) {
            return Err(format!("unused lint deviation: {} {}::{}", key.0, key.1, key.2).into());
        }
    }
    Ok(())
}

fn effective(
    lints: &Table,
    defaults: &BTreeMap<LintKey, String>,
    source: &str,
    package: &str,
) -> PolicyResult<BTreeMap<LintKey, String>> {
    if lints.get("workspace").and_then(Value::as_bool) == Some(true) {
        config::keys(lints, &["workspace"])?;
        return Ok(defaults.clone());
    }
    if !source.lines().any(|line| {
        line.trim_start().starts_with('#') && line.contains("KNOWN DIVERGENCE FROM WORKSPACE")
    }) {
        return Err(format!(
            "{package} restates lint tables without KNOWN DIVERGENCE FROM WORKSPACE"
        )
        .into());
    }
    flatten(lints)
}

fn flatten(lints: &Table) -> PolicyResult<BTreeMap<LintKey, String>> {
    config::keys(lints, &["rust", "clippy", "rustdoc"])?;
    let mut result = BTreeMap::new();
    for (group, entries) in lints {
        let group = LintGroup::parse(group)?;
        let entries = entries.as_table().ok_or("lint group must be a table")?;
        for (name, setting) in entries {
            let _previous = result.insert((group, name.clone()), level(setting)?);
        }
    }
    Ok(result)
}

fn level(setting: &Value) -> PolicyResult<String> {
    let (level, priority) = match setting {
        Value::String(level) => (level.as_str(), 0),
        Value::Table(record) => {
            config::keys(record, &["level", "priority"])?;
            let priority = match record.get("priority") {
                None => 0,
                Some(value) => value
                    .as_integer()
                    .ok_or("lint priority must be an integer")?,
            };
            (config::string(record, "level")?, priority)
        }
        _ => return Err("lint setting must be a level or a table".into()),
    };
    if !["allow", "warn", "force-warn", "deny", "forbid"].contains(&level) {
        return Err(format!("unknown Cargo lint level {level}").into());
    }
    Ok(if priority == 0 {
        level.to_owned()
    } else {
        format!("{level}:{priority}")
    })
}
