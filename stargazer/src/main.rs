// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Stargazer CLI entry point.

// The bin target is its own compilation unit and sees every crate
// dependency; acknowledge the ones consumed only by the library so
// `unused_crate_dependencies` stays silent here.
use dstar_gateway as _;
use dstar_gateway_core as _;
use mbelib_rs as _;
use reqwest as _;
use serde as _;
use serde_json as _;
#[cfg(test)]
use tempfile as _;
use thiserror as _;
use toml as _;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

/// D-STAR reflector voice recorder and activity survey.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the TOML configuration file (recording mode).
    #[arg(long, default_value = "stargazer.toml")]
    config: PathBuf,
    /// Enable per-frame debug logging.
    #[arg(long)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// Subcommands beyond the default recording mode.
#[derive(Debug, Subcommand)]
enum Cmd {
    /// Poll the DPLUSMON network feed and archive every observed
    /// transmission (raw responses + deduplicated event log).
    Survey {
        /// Archive directory.
        #[arg(long, default_value = "survey")]
        out: PathBuf,
        /// Poll interval in seconds (minimum 30).
        #[arg(long, default_value_t = 60)]
        interval: u64,
        /// Poll exactly once and exit.
        #[arg(long)]
        once: bool,
    },
    /// Rank reflector modules by archived voice activity.
    Report {
        /// Archive directory (as used by `survey`).
        #[arg(long, default_value = "survey")]
        out: PathBuf,
        /// Ranking window in hours.
        #[arg(long, default_value_t = 24)]
        window_hours: u64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    init_tracing(args.verbose);

    match args.cmd {
        None => record(&args.config).await,
        Some(Cmd::Survey {
            out,
            interval,
            once,
        }) => survey(&out, interval, once).await,
        Some(Cmd::Report { out, window_hours }) => report(&out, window_hours),
    }
}

/// Default mode: record configured reflector targets.
async fn record(config_path: &std::path::Path) -> ExitCode {
    let config = match stargazer::config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(path = %config_path.display(), error = %e, "config load failed");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&config.recordings_dir) {
        tracing::error!(
            dir = %config.recordings_dir.display(),
            error = %e,
            "cannot create recordings dir"
        );
        return ExitCode::FAILURE;
    }
    tracing::info!(
        targets = config.targets.len(),
        recordings_dir = %config.recordings_dir.display(),
        "stargazer recording"
    );

    let writer = Arc::new(stargazer::writer::Writer::new(
        config.recordings_dir.clone(),
        config.write_wav,
    ));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let mut supervisors = Vec::with_capacity(config.targets.len());
    for target in config.targets.clone() {
        supervisors.push(tokio::spawn(stargazer::session::run_supervisor(
            target,
            config.callsign,
            config.local_module,
            Arc::clone(&writer),
            shutdown_rx.clone(),
        )));
    }

    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("shutdown requested"),
        Err(e) => tracing::error!(error = %e, "ctrl-c handler failed — shutting down"),
    }
    let _unused = shutdown_tx.send(true);

    let drain = async {
        for sup in supervisors {
            let _unused = sup.await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .is_err()
    {
        tracing::warn!("supervisors did not drain within 5s — exiting anyway");
    }
    ExitCode::SUCCESS
}

/// Survey mode: poll the DPLUSMON feed gently and archive everything.
async fn survey(out: &std::path::Path, interval: u64, once: bool) -> ExitCode {
    let interval = interval.max(stargazer::survey::MIN_INTERVAL_SECS);
    let mut surveyor = match stargazer::survey::Surveyor::new(out) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "survey init failed");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        archive = %out.display(),
        interval_s = interval,
        seeded_events = surveyor.total_seen(),
        "surveying DPLUS activity via DPLUSMON"
    );

    let mut consecutive_errors: u32 = 0;
    loop {
        match surveyor.poll_once().await {
            Err(e) => {
                // Archive I/O failure — the archive is the point.
                tracing::error!(error = %e, "survey archive failure");
                return ExitCode::FAILURE;
            }
            Ok(rec) => {
                if let Some(err) = &rec.error {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    tracing::warn!(error = %err, consecutive = consecutive_errors, "poll failed");
                } else {
                    consecutive_errors = 0;
                    tracing::info!(
                        rows = rec.rows,
                        new = rec.new_events,
                        total = surveyor.total_seen(),
                        gap_risk = rec.gap_risk,
                        "poll ok"
                    );
                    if rec.gap_risk {
                        tracing::warn!(
                            "feed window rolled over between polls — some rows may be missing"
                        );
                    }
                }
            }
        }
        if once {
            return ExitCode::SUCCESS;
        }
        // Back off politely on repeated errors: up to 16× interval.
        let factor = 1u64 << consecutive_errors.min(4);
        let sleep = Duration::from_secs(interval.saturating_mul(factor));
        tokio::select! {
            () = tokio::time::sleep(sleep) => {}
            result = tokio::signal::ctrl_c() => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "ctrl-c handler failed — exiting");
                }
                tracing::info!("survey stopped");
                return ExitCode::SUCCESS;
            }
        }
    }
}

/// Report mode: rank reflector modules from the archived events.
fn report(out: &std::path::Path, window_hours: u64) -> ExitCode {
    let archive = stargazer::survey::Archive::new(out);
    let events = match archive.load_events() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, archive = %out.display(), "cannot read archive");
            return ExitCode::FAILURE;
        }
    };
    let hours = i64::try_from(window_hours).unwrap_or(i64::MAX);
    let since = chrono::Utc::now() - chrono::TimeDelta::hours(hours);
    let ranked = stargazer::survey::rank_activity(&events, since);

    println!(
        "DPLUS voice activity — last {window_hours}h ({} events archived)",
        events.len()
    );
    // Manually aligned to the row format below.
    println!("REFLECTOR  MODULE TRANSMISSIONS  STATIONS  LAST HEARD (UTC)");
    for row in &ranked {
        println!(
            "{:<10} {:<6} {:>13} {:>9}  {}",
            row.reflector,
            row.module,
            row.transmissions,
            row.distinct_callsigns,
            row.last_heard.format("%Y-%m-%d %H:%M:%S")
        );
    }
    if ranked.is_empty() {
        println!("(no reflector activity in window — is the survey running?)");
    }
    ExitCode::SUCCESS
}

fn init_tracing(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    let default = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
