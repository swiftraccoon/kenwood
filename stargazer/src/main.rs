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
    /// Fetch reflector-published recordings (reference-decoded MP3s,
    /// sidecars, gap-fill packet logs) that pair with local
    /// recordings, into per-date `published/` subdirectories.
    Harvest {
        /// Recordings directory (as written by the recorder).
        #[arg(long, default_value = "recordings")]
        recordings: PathBuf,
        /// Date to harvest, YYYY-MM-DD (UTC). Defaults to today —
        /// published retention is short, so harvest same-day.
        #[arg(long, value_parser = parse_date)]
        date: Option<chrono::NaiveDate>,
        /// Restrict to one reflector-module directory (repeatable,
        /// e.g. `--target REF030-C`). Default: every directory.
        #[arg(long)]
        target: Vec<String>,
        /// Dashboard base URL override (testing / non-REF systems).
        #[arg(long)]
        base_url: Option<String>,
        /// Maximum downloads this run.
        #[arg(long)]
        limit: Option<usize>,
        /// Match and report only — download and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Parse a `--date` argument.
fn parse_date(s: &str) -> Result<chrono::NaiveDate, String> {
    s.parse().map_err(|e| format!("{e} (expected YYYY-MM-DD)"))
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
        Some(Cmd::Harvest {
            recordings,
            date,
            target,
            base_url,
            limit,
            dry_run,
        }) => harvest(&recordings, date, &target, base_url, limit, dry_run).await,
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

/// Harvest mode: fetch published recordings that pair with ours.
async fn harvest(
    recordings: &std::path::Path,
    date: Option<chrono::NaiveDate>,
    targets: &[String],
    base_url: Option<String>,
    limit: Option<usize>,
    dry_run: bool,
) -> ExitCode {
    use stargazer::harvest::{HarvestOptions, Harvester, split_target};

    let date = date.unwrap_or_else(|| chrono::Utc::now().date_naive());
    let harvester = match Harvester::new(HarvestOptions {
        base_url: base_url.clone(),
        limit,
        dry_run,
        ..HarvestOptions::default()
    }) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "harvester init failed");
            return ExitCode::FAILURE;
        }
    };

    let mut names: Vec<String> = if targets.is_empty() {
        // Auto-walk: harvest every <SYSTEM>-<MODULE> directory that
        // has a reachable dashboard scheme; skip the rest quietly.
        let entries = match std::fs::read_dir(recordings) {
            Ok(rd) => rd,
            Err(e) => {
                tracing::error!(
                    dir = %recordings.display(),
                    error = %e,
                    "cannot read recordings dir"
                );
                return ExitCode::FAILURE;
            }
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|name| match split_target(name) {
                None => false,
                Some((system, _)) => {
                    let derivable = base_url.is_some()
                        || stargazer::harvest::derived_base_url(&system).is_some();
                    if !derivable {
                        tracing::info!(
                            target = %name,
                            "skipped: no derivable dashboard URL (pass --target + --base-url)"
                        );
                    }
                    derivable
                }
            })
            .collect()
    } else {
        targets.to_vec()
    };
    names.sort();
    names.dedup();
    if names.is_empty() {
        tracing::warn!(
            dir = %recordings.display(),
            "no <SYSTEM>-<MODULE> directories to harvest"
        );
        return ExitCode::SUCCESS;
    }

    let mut hard_failures = 0usize;
    for name in &names {
        match harvester.harvest_dir(recordings, name, date).await {
            Err(e) => {
                hard_failures += 1;
                tracing::error!(target = %name, error = %e, "harvest failed");
            }
            Ok(rec) => print_harvest_run(&rec),
        }
    }
    if hard_failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One human-readable coverage line per harvested target.
fn print_harvest_run(rec: &stargazer::harvest::RunRecord) {
    if let Some(err) = &rec.error {
        println!("{} {}: listing unavailable — {err}", rec.target, rec.date);
        return;
    }
    if rec.http_status == Some(404) {
        println!("{} {}: nothing published", rec.target, rec.date);
        return;
    }
    let pct = if rec.published_tx > 0 {
        100 * rec.matched / rec.published_tx
    } else {
        0
    };
    let action = if rec.dry_run {
        format!("dry run, would fetch {}", rec.planned)
    } else {
        format!(
            "downloaded {} · failed {} · already had {}{}",
            rec.downloaded,
            rec.failed,
            rec.skipped_existing,
            if rec.truncated {
                " · truncated by --limit"
            } else {
                ""
            }
        )
    };
    println!(
        "{} {}: published {} · local {} · matched {} ({pct}%) · salvage {} · local-only {} — {action}",
        rec.target,
        rec.date,
        rec.published_tx,
        rec.local_recordings,
        rec.matched,
        rec.published_only,
        rec.local_only,
    );
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
