//! Exact workspace membership, package names and standalone fuzz coverage.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use toml::{Table, Value};

use crate::config::{self, Policy};
use crate::{PolicyResult, lints};

pub(crate) fn check(root: &Path, policy: &Policy) -> PolicyResult<()> {
    let document = fs::read_to_string(root.join("Cargo.toml"))?.parse::<Table>()?;
    let workspace = document
        .get("workspace")
        .and_then(Value::as_table)
        .ok_or("root manifest needs a workspace table")?;
    let members = workspace
        .get("members")
        .and_then(Value::as_array)
        .ok_or("workspace needs explicit members")?;
    let paths = members
        .iter()
        .map(|value| {
            config::relative_path(value.as_str().ok_or("workspace member must be a string")?)
        })
        .collect::<PolicyResult<BTreeSet<_>>>()?;
    if paths.len() != members.len() {
        return Err("duplicate root workspace member".into());
    }
    let listed: BTreeSet<_> = policy
        .packages
        .iter()
        .map(|package| package.path.clone())
        .collect();
    let names: BTreeSet<_> = policy
        .packages
        .iter()
        .map(|package| &package.name)
        .collect();
    if listed.len() != policy.packages.len() || names.len() != policy.packages.len() {
        return Err("duplicate package name or path in rust policy".into());
    }
    if paths != listed {
        return Err(format!(
            "workspace member inventory differs: manifest {paths:?}, policy {listed:?}"
        )
        .into());
    }
    check_fuzz(root, policy)?;
    for package in &policy.packages {
        check_package(root, package)?;
    }
    let defaults = workspace
        .get("lints")
        .and_then(Value::as_table)
        .ok_or("workspace needs lint tables")?;
    lints::check(root, defaults, policy)
}

fn check_package(root: &Path, package: &config::Package) -> PolicyResult<()> {
    let manifest =
        fs::read_to_string(root.join(&package.path).join("Cargo.toml"))?.parse::<Table>()?;
    let metadata = manifest
        .get("package")
        .and_then(Value::as_table)
        .ok_or("member manifest needs package metadata")?;
    if config::string(metadata, "name")? != package.name {
        return Err(format!(
            "package name differs from inventory at {}",
            package.path.display()
        )
        .into());
    }
    Ok(())
}

fn check_fuzz(root: &Path, policy: &Policy) -> PolicyResult<()> {
    let found: BTreeSet<_> = policy
        .packages
        .iter()
        .map(|package| package.path.join("fuzz/Cargo.toml"))
        .filter(|manifest| root.join(manifest).is_file())
        .collect();
    if found != policy.fuzz {
        return Err(format!(
            "fuzz manifest inventory differs: found {found:?}, policy {:?}",
            policy.fuzz
        )
        .into());
    }
    for path in &found {
        let manifest = fs::read_to_string(root.join(path))?.parse::<Table>()?;
        if !manifest.get("workspace").is_some_and(Value::is_table) {
            return Err(format!("{} needs its standalone workspace table", path.display()).into());
        }
    }
    Ok(())
}
