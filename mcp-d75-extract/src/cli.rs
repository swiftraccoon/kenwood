//! Command-line interface: the `extract` and `diff` subcommands.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::diff::diff_manifests;
use crate::error::{Result, extract_error};
use crate::extract::{BuildOptions, build_manifest};
use crate::manifest::{Manifest, json_text, parse_manifest};
use crate::model::model_by_id;
use crate::rustgen::rust_text;

/// Extract MCP memory-map manifests from ILSpy-decompiled programs.
#[derive(Debug, Parser)]
#[command(name = "mcp-d75-extract")]
pub struct Cli {
    /// Subcommand.
    #[command(subcommand)]
    pub command: Command,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Decompile (or read) a program and write its manifest.
    Extract(ExtractArgs),
    /// Report layout changes between two manifests of one radio.
    Diff(DiffArgs),
}

/// Arguments of `diff`.
#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Manifest of the earlier release.
    pub old: PathBuf,
    /// Manifest of the later release.
    pub new: PathBuf,
}

/// Arguments of `extract`.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("source").required(true).multiple(false)))]
pub struct ExtractArgs {
    /// Radio spec id: `thd75` or `tmd750`.
    #[arg(long)]
    pub model: String,
    /// Existing `ILSpy` project directory.
    #[arg(long, group = "source")]
    pub source_dir: Option<PathBuf>,
    /// Program executable to decompile.
    #[arg(long, group = "source")]
    pub assembly: Option<PathBuf>,
    /// Path to ilspycmd (used with --assembly).
    #[arg(long)]
    pub ilspycmd: Option<String>,
    /// Declared MCP marketing version, e.g. `1.03`.
    #[arg(long)]
    pub mcp_version: String,
    /// Declared firmware target, e.g. `1.03`.
    #[arg(long)]
    pub firmware: String,
    /// Optional UTF-16 language file for option labels.
    #[arg(long)]
    pub language_file: Option<PathBuf>,
    /// JSON manifest output.
    #[arg(long)]
    pub output: PathBuf,
    /// Optional generated Rust menu-field registry output.
    #[arg(long)]
    pub rust_output: Option<PathBuf>,
    /// Require the spec's reviewed counts.
    #[arg(long)]
    pub strict_known_layout: bool,
    /// Fail if generated content differs instead of writing files.
    #[arg(long)]
    pub check: bool,
}

/// Write generated content, or in check mode verify it matches.
///
/// # Errors
///
/// In check mode, returns an error when the output file is missing,
/// unreadable, or differs from `content`; in write mode, when the file or
/// its parent directory cannot be created.
pub fn write_or_check(path: &Path, content: &str, check: bool) -> Result<()> {
    if check {
        let existing = std::fs::read_to_string(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                extract_error!("--check output does not exist: {}", path.display())
            } else {
                extract_error!("cannot read {}: {error}", path.display())
            }
        })?;
        if existing != content {
            return Err(extract_error!(
                "generated output differs: {}",
                path.display()
            ));
        }
        return Ok(());
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| extract_error!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, content)
        .map_err(|error| extract_error!("cannot write {}: {error}", path.display()))
}

fn find_ilspycmd(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(explicit) = explicit
        && !explicit.is_empty()
    {
        return Ok(PathBuf::from(explicit));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let candidate = directory.join("ilspycmd");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(extract_error!(
        "ilspycmd not found; install it or pass --ilspycmd"
    ))
}

fn decompile(assembly: &Path, output_dir: &Path, ilspycmd: Option<&str>) -> Result<()> {
    let status = std::process::Command::new(find_ilspycmd(ilspycmd)?)
        .arg("--disable-updatecheck")
        .arg("-p")
        .arg("-o")
        .arg(output_dir)
        .arg(assembly)
        .status()
        .map_err(|error| extract_error!("cannot run ilspycmd: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(extract_error!(
            "ILSpy decompilation failed with exit code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

/// Run `extract`.
///
/// # Errors
///
/// Returns an error for an unknown model, a missing assembly, a failed
/// decompilation or extraction, or an output that cannot be written (or,
/// with `--check`, does not match).
pub fn run_extract(args: &ExtractArgs) -> Result<()> {
    let model = model_by_id(&args.model).ok_or_else(|| {
        extract_error!("unknown model {:?}; expected thd75 or tmd750", args.model)
    })?;
    let options = BuildOptions {
        model,
        mcp_version: args.mcp_version.clone(),
        firmware_target: args.firmware.clone(),
        language_file: args.language_file.clone(),
        strict_known_layout: args.strict_known_layout,
    };
    let manifest = if let Some(source_dir) = args.source_dir.as_deref() {
        build_manifest(source_dir, &options)?
    } else {
        let assembly = args
            .assembly
            .as_deref()
            .ok_or_else(|| extract_error!("either --source-dir or --assembly is required"))?;
        if !assembly.is_file() {
            return Err(extract_error!("assembly not found: {}", assembly.display()));
        }
        let temporary = tempfile::Builder::new()
            .prefix("mcp-extract-ilspy-")
            .tempdir()
            .map_err(|error| extract_error!("cannot create temporary directory: {error}"))?;
        decompile(assembly, temporary.path(), args.ilspycmd.as_deref())?;
        build_manifest(temporary.path(), &options)?
    };
    write_or_check(&args.output, &json_text(&manifest)?, args.check)?;
    if let Some(rust_output) = args.rust_output.as_deref() {
        write_or_check(rust_output, &rust_text(&manifest)?, args.check)?;
    }
    Ok(())
}

/// Run `diff`; returns the exit code (0 identical, 1 differences).
///
/// # Errors
///
/// Returns an error when either manifest cannot be read or parsed, or when
/// the manifests describe different radios.
pub fn run_diff(args: &DiffArgs) -> Result<i32> {
    let read = |path: &Path| -> Result<Manifest> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| extract_error!("cannot read {}: {error}", path.display()))?;
        parse_manifest(&text)
    };
    let report = diff_manifests(&read(&args.old)?, &read(&args.new)?)?;
    print!("{report}");
    Ok(i32::from(report.differences > 0))
}

/// CLI entry point returning the process exit code.
pub fn main_with_args<I, T>(arguments: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let with_program = std::iter::once(std::ffi::OsString::from("mcp-d75-extract"))
        .chain(arguments.into_iter().map(Into::into));
    let cli = match Cli::try_parse_from(with_program) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            drop(error.print());
            return code;
        }
    };
    match &cli.command {
        Command::Extract(args) => match run_extract(args) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        },
        Command::Diff(args) => match run_diff(args) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("error: {error}");
                2
            }
        },
    }
}
