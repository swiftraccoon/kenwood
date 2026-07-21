//! Thin binary shim over the extractor library.

// Each binary is a separate compilation unit that sees every package
// dependency; the library consumes them all, this shim only calls into it.
use clap as _;
use fancy_regex as _;
use regex as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;
use thiserror as _;

fn main() -> std::process::ExitCode {
    let code = mcp_d75_extract::main_with_args(std::env::args_os().skip(1));
    std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
}
