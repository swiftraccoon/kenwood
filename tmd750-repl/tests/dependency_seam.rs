//! The TM-D750 application uses model-neutral protocol runtimes directly.

use clap as _;
use dirs_next as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use kenwood_tmd750 as _;
use kenwood_transport as _;
use mmdvm as _;
#[cfg(unix)]
use nix as _;
#[cfg(unix)]
use rustix as _;
use rustyline as _;
use serde as _;
use serde_json as _;
use tempfile as _;
use thiserror as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rust_sources(directory: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            rust_sources(&path, found)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    Ok(())
}

#[test]
fn application_sources_and_manifest_do_not_depend_on_another_radio_model() -> TestResult {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&manifest_dir.join("src"), &mut sources)?;
    let mut offenders = Vec::new();
    for path in &sources {
        if std::fs::read_to_string(path)?.contains("kenwood_thd75") {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "application source must use the shared protocol runtime: {offenders:?}"
    );
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    assert!(
        !manifest.contains("kenwood-thd75"),
        "the application must not acquire another model's controller or native helpers"
    );
    Ok(())
}
