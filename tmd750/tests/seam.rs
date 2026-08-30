//! The TH-D75 crate is reachable only through `src/transport.rs`.

use kenwood_thd75 as _;
use kenwood_tmd750 as _;
use mcp_d75_extract as _;
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
fn only_the_transport_seam_names_thd75() -> TestResult {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    rust_sources(&src, &mut sources)?;
    let seam = src.join("transport.rs");
    let mut offenders = Vec::new();
    for path in &sources {
        let text = std::fs::read_to_string(path)?;
        if *path != seam && text.contains("thd75") {
            offenders.push(path.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "thd75 is named outside the seam: {offenders:?}"
    );
    let seam_text = std::fs::read_to_string(&seam)?;
    for forbidden in [
        "kenwood_thd75::radio",
        "kenwood_thd75::memory",
        "kenwood_thd75::protocol",
        "kenwood_thd75::types",
    ] {
        assert!(
            !seam_text.contains(forbidden),
            "the seam imports {forbidden}"
        );
    }
    Ok(())
}
