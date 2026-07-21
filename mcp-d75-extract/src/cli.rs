//! Command-line interface: argument parsing, decompilation, output writing.

use std::path::{Path, PathBuf};

use clap::Parser;

use crate::error::{Result, extract_error};
use crate::rustgen::rust_text;
use crate::schema::{BuildOptions, build_schema, json_text};

/// Extract MCP-D75 menu writes from ILSpy-decompiled C#.
#[derive(Debug, Parser)]
#[command(
    name = "mcp-d75-extract",
    group(clap::ArgGroup::new("source").required(true).multiple(false))
)]
pub struct Cli {
    /// Existing `ILSpy` project directory.
    #[arg(long, group = "source")]
    pub source_dir: Option<PathBuf>,
    /// MCP-D75.exe to decompile.
    #[arg(long, group = "source")]
    pub assembly: Option<PathBuf>,
    /// Path to ilspycmd (used with --assembly).
    #[arg(long)]
    pub ilspycmd: Option<String>,
    /// Optional UTF-16 MCP-D75 Language/English.lng for option labels.
    #[arg(long)]
    pub language_file: Option<PathBuf>,
    /// JSON manifest output.
    #[arg(long)]
    pub output: PathBuf,
    /// Optional generated Rust menu-field registry output.
    #[arg(long)]
    pub rust_output: Option<PathBuf>,
    /// Require the reviewed 134/17/85/31 direct-operation counts.
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

/// Locate ilspycmd explicitly or on `PATH`, or fail with guidance.
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

/// Run ilspycmd to decompile the assembly into `output_dir`.
fn decompile(assembly: &Path, output_dir: &Path, ilspycmd: Option<&str>) -> Result<()> {
    let command = find_ilspycmd(ilspycmd)?;
    let status = std::process::Command::new(command)
        .arg("--disable-updatecheck")
        .arg("-p")
        .arg("-o")
        .arg(output_dir)
        .arg(assembly)
        .status()
        .map_err(|error| extract_error!("cannot run ilspycmd: {error}"))?;
    if !status.success() {
        return Err(extract_error!(
            "ILSpy decompilation failed with exit code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Build the schema from the selected source and emit or check outputs.
///
/// # Errors
///
/// Returns an error when decompilation or extraction fails, or when an
/// output cannot be written (or, with `--check`, does not match).
pub fn run(cli: &Cli) -> Result<()> {
    let options = BuildOptions {
        strict_known_layout: cli.strict_known_layout,
        language_file: cli.language_file.clone(),
    };
    let schema = if let Some(source_dir) = cli.source_dir.as_deref() {
        build_schema(source_dir, &options)?
    } else {
        let assembly = cli
            .assembly
            .as_deref()
            .ok_or_else(|| extract_error!("either --source-dir or --assembly is required"))?;
        if !assembly.is_file() {
            return Err(extract_error!("assembly not found: {}", assembly.display()));
        }
        let temporary = tempfile::Builder::new()
            .prefix("mcp-d75-ilspy-")
            .tempdir()
            .map_err(|error| extract_error!("cannot create temporary directory: {error}"))?;
        decompile(assembly, temporary.path(), cli.ilspycmd.as_deref())?;
        build_schema(temporary.path(), &options)?
    };
    write_or_check(&cli.output, &json_text(&schema)?, cli.check)?;
    if let Some(rust_output) = cli.rust_output.as_deref() {
        write_or_check(rust_output, &rust_text(&schema)?, cli.check)?;
    }
    Ok(())
}

/// CLI entry point: extract, then write or verify the outputs.
///
/// Takes the arguments *after* the program name and returns the process
/// exit code (0 on success, 1 on extraction failure, clap's own code on
/// usage errors).
pub fn main_with_args<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let with_program = std::iter::once(std::ffi::OsString::from("mcp-d75-extract"))
        .chain(argv.into_iter().map(Into::into));
    let cli = match Cli::try_parse_from(with_program) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            drop(error.print());
            return code;
        }
    };
    match run(&cli) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}
