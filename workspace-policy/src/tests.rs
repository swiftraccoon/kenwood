use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use toml::{Table, Value};

use crate::{PolicyResult, config, verify};

const ROOT_MANIFEST: &str = r#"
[workspace]
members = ["alpha"]

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }
nursery = { level = "deny", priority = -1 }
cargo = { level = "deny", priority = -1 }
"#;
const MEMBER_MANIFEST: &str = r#"
[package]
name = "alpha"
version = "0.1.0"

[lints]
workspace = true
"#;
const POLICY: &str = r#"
version = 1
fuzz = []

[[packages]]
name = "alpha"
path = "alpha"
"#;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> PolicyResult<Self> {
        let root = std::env::temp_dir().join(format!(
            "kenwood-policy-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        let fixture = Self { root };
        fs::create_dir(fixture.root.join(".config"))?;
        fs::create_dir(fixture.root.join("alpha"))?;
        fixture.write("Cargo.toml", ROOT_MANIFEST)?;
        fixture.write("alpha/Cargo.toml", MEMBER_MANIFEST)?;
        fixture.write(".config/rust-policy.toml", POLICY)?;
        Ok(fixture)
    }

    fn write(&self, path: &str, source: &str) -> PolicyResult<()> {
        fs::write(self.root.join(path), source)?;
        Ok(())
    }

    fn check(&self) -> PolicyResult<()> {
        verify::check(&self.root, &config::Policy::read(&self.root)?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.root);
    }
}

fn rejects(result: PolicyResult<()>, diagnostic: &str) -> PolicyResult<()> {
    let error = result.err().ok_or("policy accepted an invalid fixture")?;
    assert!(
        error.to_string().contains(diagnostic),
        "expected {diagnostic:?}, received {error}"
    );
    Ok(())
}

fn restated_member(missing_docs: Option<&str>) -> PolicyResult<String> {
    let baseline = ROOT_MANIFEST.parse::<Table>()?;
    let mut lints = baseline
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
        .and_then(Value::as_table)
        .ok_or("fixture root needs lint tables")?
        .clone();
    let rust = lints
        .get_mut("rust")
        .and_then(Value::as_table_mut)
        .ok_or("fixture root needs Rust lints")?;
    match missing_docs {
        Some(level) => {
            let _previous = rust.insert("missing_docs".to_owned(), Value::String(level.to_owned()));
        }
        None => {
            let _previous = rust.remove("missing_docs");
        }
    }
    let mut member = MEMBER_MANIFEST.parse::<Table>()?;
    let _previous = member.insert("lints".to_owned(), Value::Table(lints));
    Ok(format!(
        r"# KNOWN DIVERGENCE FROM WORKSPACE: fixture.
{}",
        toml::to_string(&member)?
    ))
}

fn restated_lint(group: &str, lint: &str, setting: Option<&str>) -> PolicyResult<String> {
    let mut member = restated_member(Some("deny"))?.parse::<Table>()?;
    let entries = member
        .get_mut("lints")
        .and_then(|lints| lints.get_mut(group))
        .and_then(Value::as_table_mut)
        .ok_or("fixture needs its lint group")?;
    if let Some(setting) = setting {
        let _previous = entries.insert(lint.to_owned(), Value::String(setting.to_owned()));
    } else {
        let _previous = entries.remove(lint);
    }
    Ok(format!(
        "# KNOWN DIVERGENCE FROM WORKSPACE: fixture.\n{}",
        toml::to_string(&member)?
    ))
}

fn document_deviation(fixture: &Fixture, group: &str, lint: &str, level: &str) -> PolicyResult<()> {
    fixture.write(
        ".config/rust-policy.toml",
        &format!(
            r#"{POLICY}
[[deviations]]
package = "alpha"
group = "{group}"
lint = "{lint}"
level = "{level}"
reason = "Fixture deviation."
"#
        ),
    )
}

fn rejects_documented_clippy_weakening(lint: &str) -> PolicyResult<()> {
    for setting in [Some("allow"), Some("warn"), Some("force-warn"), None] {
        let fixture = Fixture::new()?;
        fixture.check()?;
        fixture.write("alpha/Cargo.toml", &restated_lint("clippy", lint, setting)?)?;
        document_deviation(&fixture, "clippy", lint, setting.unwrap_or("omitted"))?;
        rejects(
            fixture.check(),
            &format!("alpha must set clippy::{lint} to deny or forbid"),
        )?;
    }
    Ok(())
}

#[test]
fn documented_clippy_all_exception_cannot_weaken_the_group() -> PolicyResult<()> {
    rejects_documented_clippy_weakening("all")
}

#[test]
fn documented_clippy_pedantic_exception_cannot_weaken_the_group() -> PolicyResult<()> {
    rejects_documented_clippy_weakening("pedantic")
}

#[test]
fn documented_clippy_nursery_exception_cannot_weaken_the_group() -> PolicyResult<()> {
    rejects_documented_clippy_weakening("nursery")
}

#[test]
fn documented_clippy_cargo_exception_cannot_weaken_the_group() -> PolicyResult<()> {
    rejects_documented_clippy_weakening("cargo")
}

#[test]
fn documented_forbid_strengthening_preserves_mandatory_clippy_groups() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    for lint in ["all", "pedantic", "nursery", "cargo"] {
        fixture.write(
            "alpha/Cargo.toml",
            &restated_lint("clippy", lint, Some("forbid"))?,
        )?;
        document_deviation(&fixture, "clippy", lint, "forbid")?;
        fixture.check()?;
    }
    Ok(())
}

#[test]
fn documented_native_unsafe_exception_preserves_mandatory_clippy_groups() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fixture.write(
        "alpha/Cargo.toml",
        &restated_lint("rust", "unsafe_code", Some("deny"))?,
    )?;
    document_deviation(&fixture, "rust", "unsafe_code", "deny")?;
    fixture.check()
}

#[test]
fn omitted_member_fails_after_complete_inventory_passes() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    let mut manifest = ROOT_MANIFEST.parse::<Table>()?;
    let workspace = manifest
        .get_mut("workspace")
        .and_then(Value::as_table_mut)
        .ok_or("fixture needs a workspace table")?;
    let members = ["alpha", "beta"]
        .into_iter()
        .map(|name| Value::String(name.to_owned()))
        .collect();
    let _previous = workspace.insert("members".to_owned(), Value::Array(members));
    fixture.write("Cargo.toml", &toml::to_string(&manifest)?)?;
    rejects(fixture.check(), "workspace member inventory differs")
}

#[test]
fn inherited_lints_cannot_accept_a_weakened_root_policy() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    for (group, lint) in [
        ("rust", "unsafe_code"),
        ("clippy", "all"),
        ("clippy", "pedantic"),
        ("clippy", "nursery"),
        ("clippy", "cargo"),
    ] {
        let mut manifest = ROOT_MANIFEST.parse::<Table>()?;
        let entries = manifest
            .get_mut("workspace")
            .and_then(|workspace| workspace.get_mut("lints"))
            .and_then(|lints| lints.get_mut(group))
            .and_then(Value::as_table_mut)
            .ok_or("fixture needs its root lint group")?;
        let _previous = entries.insert(lint.to_owned(), Value::String("allow".to_owned()));
        fixture.write("Cargo.toml", &toml::to_string(&manifest)?)?;
        rejects(
            fixture.check(),
            &format!("workspace root must set {group}::{lint}"),
        )?;
    }
    Ok(())
}

#[test]
fn changed_lint_requires_its_exact_documented_level() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fixture.write("alpha/Cargo.toml", &restated_member(Some("warn"))?)?;
    rejects(fixture.check(), "effective warn")?;
    fixture.write(
        ".config/rust-policy.toml",
        &format!(
            r#"{POLICY}
[[deviations]]
package = "alpha"
group = "rust"
lint = "missing_docs"
level = "warn"
reason = "Fixture deviation."
"#
        ),
    )?;
    fixture.check()?;
    fixture.write("alpha/Cargo.toml", &restated_member(Some("allow"))?)?;
    rejects(fixture.check(), "effective allow")
}

#[test]
fn omitted_lint_needs_an_explicit_omission_exception() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fixture.write("alpha/Cargo.toml", &restated_member(None)?)?;
    rejects(fixture.check(), "effective omitted")?;
    fixture.write(
        ".config/rust-policy.toml",
        &format!(
            r#"{POLICY}
[[deviations]]
package = "alpha"
group = "rust"
lint = "missing_docs"
level = "omitted"
reason = "Fixture omission."
"#
        ),
    )?;
    fixture.check()
}

#[test]
fn unused_lint_exception_fails() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fixture.write(
        ".config/rust-policy.toml",
        &format!(
            r#"{POLICY}
[[deviations]]
package = "alpha"
group = "rust"
lint = "missing_docs"
level = "warn"
reason = "Stale fixture exception."
"#
        ),
    )?;
    rejects(fixture.check(), "unused lint deviation")
}

#[test]
fn restated_lints_need_the_manifest_header() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fixture.write(
        "alpha/Cargo.toml",
        r#"
[package]
name = "alpha"

[lints.rust]
missing_docs = "deny"
"#,
    )?;
    rejects(fixture.check(), "without KNOWN DIVERGENCE")
}

#[test]
fn policy_accepts_inventory_without_review_documents() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    assert!(
        !fixture.root.join("local-notes.md").exists(),
        "the fresh fixture must contain no root audit document"
    );
    assert!(
        !fixture.root.join("alpha/local-notes.md").exists(),
        "the fresh fixture must contain no package audit document"
    );
    fixture.check()
}

#[test]
fn local_review_documents_cannot_change_manifest_enforcement() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.write("local-notes.md", "Unstructured local notes.\n")?;
    fixture.write("alpha/local-notes.md", "Incomplete local notes.\n")?;
    fixture.check()?;
    fixture.write("local-notes.md", "| alpha | all requirements satisfied |\n")?;
    fixture.write("alpha/Cargo.toml", &restated_member(Some("allow"))?)?;
    rejects(fixture.check(), "effective allow")
}

#[test]
fn removed_report_and_audit_fields_are_rejected() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    let mut policy = POLICY.parse::<Table>()?;
    let _previous = policy.insert(
        "report".to_owned(),
        Value::String("local-notes.md".to_owned()),
    );
    fixture.write(".config/rust-policy.toml", &toml::to_string(&policy)?)?;
    rejects(
        config::Policy::read(&fixture.root).map(|_| ()),
        "unknown policy field report",
    )?;
    let mut policy = POLICY.parse::<Table>()?;
    let package = policy
        .get_mut("packages")
        .and_then(Value::as_array_mut)
        .and_then(|packages| packages.first_mut())
        .and_then(Value::as_table_mut)
        .ok_or("fixture needs a package")?;
    let _previous = package.insert("audit".to_owned(), Value::String("reviewed".to_owned()));
    fixture.write(".config/rust-policy.toml", &toml::to_string(&policy)?)?;
    rejects(
        config::Policy::read(&fixture.root).map(|_| ()),
        "unknown policy field audit",
    )
}

#[test]
fn source_archive_detects_unlisted_fuzz_workspace() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fs::create_dir(fixture.root.join("alpha/fuzz"))?;
    fixture.write(
        "alpha/fuzz/Cargo.toml",
        r"
[workspace]
",
    )?;
    rejects(fixture.check(), "fuzz manifest inventory differs")
}

#[test]
fn standalone_fuzz_lints_require_exact_policy_and_inheritance() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fs::create_dir(fixture.root.join("alpha/fuzz"))?;
    let mut policy = POLICY.parse::<Table>()?;
    let mut record = Table::new();
    let _previous = record.insert(
        "manifest".to_owned(),
        Value::String("alpha/fuzz/Cargo.toml".to_owned()),
    );
    let _previous = policy.insert("fuzz".to_owned(), Value::Array(vec![Value::Table(record)]));
    fixture.write(".config/rust-policy.toml", &toml::to_string(&policy)?)?;
    let baseline = ROOT_MANIFEST.parse::<Table>()?;
    let defaults = baseline
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
        .ok_or("fixture needs root lint tables")?
        .clone();
    let mut fuzz = MEMBER_MANIFEST.parse::<Table>()?;
    let mut workspace = Table::new();
    let _previous = workspace.insert("lints".to_owned(), defaults);
    let _previous = fuzz.insert("workspace".to_owned(), Value::Table(workspace));
    fixture.write("alpha/fuzz/Cargo.toml", &toml::to_string(&fuzz)?)?;
    fixture.check()?;

    let mut weakened = fuzz.clone();
    let entries = weakened
        .get_mut("workspace")
        .and_then(|workspace| workspace.get_mut("lints"))
        .and_then(|lints| lints.get_mut("clippy"))
        .and_then(Value::as_table_mut)
        .ok_or("fixture needs Clippy lints")?;
    let _previous = entries.remove("pedantic");
    fixture.write("alpha/fuzz/Cargo.toml", &toml::to_string(&weakened)?)?;
    rejects(fixture.check(), "clippy::pedantic differs")?;

    let _previous = fuzz.remove("lints");
    fixture.write("alpha/fuzz/Cargo.toml", &toml::to_string(&fuzz)?)?;
    rejects(fixture.check(), "needs inherited lint tables")
}

#[test]
fn policy_rejects_unknown_fields_and_parent_paths() -> PolicyResult<()> {
    let fixture = Fixture::new()?;
    fixture.check()?;
    fixture.write(
        ".config/rust-policy.toml",
        &format!(
            r"{POLICY}
typo = true
"
        ),
    )?;
    rejects(
        config::Policy::read(&fixture.root).map(|_| ()),
        "unknown policy field",
    )?;
    rejects(
        config::relative_path("../outside").map(|_| ()),
        "portable relative path",
    )
}
