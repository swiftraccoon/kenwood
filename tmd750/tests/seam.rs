//! Model code must not depend on the TH-D75 library for shared transport I/O.

use kenwood_schema as _;
use kenwood_tmd750 as _;
use kenwood_transport as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

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
fn model_sources_and_manifest_do_not_depend_on_thd75() -> TestResult {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("src");
    let mut sources = Vec::new();
    rust_sources(&src, &mut sources)?;
    let mut offenders = Vec::new();
    for path in &sources {
        let text = std::fs::read_to_string(path)?;
        if text.contains("thd75") {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "model source depends on the TH-D75 library: {offenders:?}"
    );
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))?;
    assert!(
        !manifest.contains("thd75"),
        "the model manifest must depend directly on kenwood-transport, never TH-D75",
    );
    Ok(())
}
