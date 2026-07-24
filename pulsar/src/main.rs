// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Pulsar command-line entry point.

// The binary is a separate compilation unit. These dependencies are
// used by the library target and acknowledged here so the workspace's
// `unused_crate_dependencies` lint remains useful.
use chrono as _;
use dmr_rewind as _;
use serde as _;
use serde_json as _;
#[cfg(test)]
use tempfile as _;
use thiserror as _;
use toml as _;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Receive-only `BrandMeister` DMR call recorder.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the Pulsar TOML configuration.
    #[arg(long, global = true, default_value = "pulsar.toml")]
    config: PathBuf,
    /// Enable protocol-level debug logging.
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

/// Commands beyond the default recording mode.
#[derive(Debug, Subcommand)]
enum Command {
    /// Validate configuration and referenced password variables.
    Check,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    init_tracing(args.verbose);

    let config = match pulsar::config::load(&args.config) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(
                path = %args.config.display(),
                error = %error,
                "configuration failed"
            );
            return ExitCode::FAILURE;
        }
    };

    match args.command {
        Some(Command::Check) => check(&config),
        None => match pulsar::session::record(config).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(error = %error, "recorder failed");
                ExitCode::FAILURE
            }
        },
    }
}

fn check(config: &pulsar::config::Config) -> ExitCode {
    for master in &config.masters {
        if let Err(error) = master.load_password() {
            tracing::error!(master = %master.name, error = %error, "credential check failed");
            return ExitCode::FAILURE;
        }
    }
    let subscriptions = config.masters.iter().fold(0usize, |total, master| {
        total
            .saturating_add(master.talkgroups.len())
            .saturating_add(master.private_ids.len())
    });
    println!(
        "valid: {} master connection(s), {subscriptions} subscription(s), output {}, max {} capture records/call",
        config.masters.len(),
        config.recordings_dir.display(),
        config.max_capture_records_per_call.get()
    );
    ExitCode::SUCCESS
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;

    let fallback = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
