//! Model code must not re-export the shared crates' types; consumers import
//! them from the shared crate.

// Deps visible to every kenwood-thd75 test target but unused here.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kenwood_schema as _;
use kenwood_thd75 as _;
use kenwood_transport as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Shared workspace crates whose types are imported canonically, never re-exported.
const SHARED_CRATES: [&str; 4] = ["kenwood_schema", "kenwood_transport", "mmdvm", "mmdvm_core"];

/// The shared crate a `pub use` line re-exports from, if any.
fn re_exported_crate(line: &str) -> Option<&'static str> {
    let path = line.trim_start().strip_prefix("pub use ")?;
    SHARED_CRATES.into_iter().find(|shared| {
        path.strip_prefix(shared).is_some_and(|rest| {
            !rest.starts_with(|character: char| {
                character == '_' || character.is_ascii_alphanumeric()
            })
        })
    })
}

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
fn model_sources_never_re_export_shared_crate_types() -> TestResult {
    let mut sources = Vec::new();
    rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    )?;
    let mut offenders = Vec::new();
    for path in &sources {
        for line in std::fs::read_to_string(path)?.lines() {
            if let Some(shared) = re_exported_crate(line) {
                offenders.push(format!("{}: {shared}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "model source re-exports a shared crate's types; import them from the shared crate: {offenders:?}"
    );
    Ok(())
}
