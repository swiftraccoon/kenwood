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
        /// Operator callsign advertised in the User-Agent so server
        /// owners can identify and contact who is fetching (default:
        /// the callsign from --config when that file exists).
        #[arg(long)]
        operator: Option<String>,
    },
    /// Reconstruct recordings from published dvrec packet logs for
    /// transmissions with no locally captured twin (salvage from
    /// recorder-offline windows and unlinked reflectors).
    ImportDvrec {
        /// Recordings directory (as written by the recorder).
        #[arg(long, default_value = "recordings")]
        recordings: PathBuf,
    },
    /// Control the running recorder: free or reclaim individual
    /// reflector slots and apply config target changes, all without
    /// restarting (and without touching the other links).
    Ctl {
        #[command(subcommand)]
        action: CtlAction,
    },
}

/// Actions for `stargazer ctl`.
#[derive(Debug, Subcommand)]
enum CtlAction {
    /// Show every configured target's state.
    Status,
    /// Unlink one target and keep its slot free (persists across
    /// recorder restarts until `enable`).
    Disable {
        /// Target key, e.g. `REF030-C`.
        target: String,
    },
    /// Relink a disabled target.
    Enable {
        /// Target key, e.g. `REF030-C`.
        target: String,
    },
    /// Re-read the config file and start/stop supervisors to match
    /// its target list.
    Reload,
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
            operator,
        }) => {
            // Identify the responsible operator in the User-Agent —
            // explicit flag first, recorder config as the fallback.
            let operator = operator.or_else(|| {
                stargazer::config::load(&args.config)
                    .ok()
                    .map(|c| c.callsign.as_str().trim_end().to_string())
            });
            let options = stargazer::harvest::HarvestOptions {
                base_url,
                limit,
                dry_run,
                operator,
                ..stargazer::harvest::HarvestOptions::default()
            };
            harvest(&recordings, date, &target, options).await
        }
        Some(Cmd::ImportDvrec { recordings }) => import_dvrec(&recordings),
        Some(Cmd::Ctl { action }) => ctl(&args.config, action).await,
    }
}

/// Import salvaged dvrec packet logs as recordings.
fn import_dvrec(recordings: &std::path::Path) -> ExitCode {
    let writer = stargazer::writer::Writer::new(recordings.to_path_buf(), true);
    match stargazer::dvrec::import_tree(recordings, &writer) {
        Err(e) => {
            tracing::error!(error = %e, "dvrec import failed");
            ExitCode::FAILURE
        }
        Ok(summary) => {
            println!(
                "imported {} · already captured {} · kerchunks {} · failed {}",
                summary.imported,
                summary.skipped_existing,
                summary.skipped_voiceless,
                summary.failed
            );
            if summary.failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

/// Send one control command to the running recorder and print the
/// response.
async fn ctl(config_path: &std::path::Path, action: CtlAction) -> ExitCode {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let config = match stargazer::config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(path = %config_path.display(), error = %e, "config load failed");
            return ExitCode::FAILURE;
        }
    };
    let socket = config.recordings_dir.join(stargazer::control::SOCKET_FILE);
    let line = match action {
        CtlAction::Status => "status".to_string(),
        CtlAction::Disable { target } => format!("disable {target}"),
        CtlAction::Enable { target } => format!("enable {target}"),
        CtlAction::Reload => "reload".to_string(),
    };
    let mut stream = match tokio::net::UnixStream::connect(&socket).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                socket = %socket.display(),
                error = %e,
                "cannot reach the recorder — is it running (with control support)?"
            );
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = stream.write_all(format!("{line}\n").as_bytes()).await {
        tracing::error!(error = %e, "control request failed");
        return ExitCode::FAILURE;
    }
    let mut response = String::new();
    if let Err(e) = stream.read_to_string(&mut response).await {
        tracing::error!(error = %e, "control response failed");
        return ExitCode::FAILURE;
    }
    print!("{response}");
    ExitCode::SUCCESS
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
    let mut coordinator =
        match stargazer::control::Coordinator::new(config, config_path.to_path_buf(), writer) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "coordinator init failed");
                return ExitCode::FAILURE;
            }
        };
    // Single-instance guard BEFORE any reflector is touched: two
    // recorders share one callsign and would displace each other on
    // every slot, so a second instance must die without connecting.
    let socket_path = coordinator.socket_path();
    if stargazer::control::recorder_already_running(&socket_path).await {
        tracing::error!(
            socket = %socket_path.display(),
            "another recorder is already running — refusing to start a second"
        );
        return ExitCode::FAILURE;
    }
    let _unused = std::fs::remove_file(&socket_path); // stale socket after a crash
    let listener = match tokio::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(socket = %socket_path.display(), error = %e, "control socket bind failed");
            return ExitCode::FAILURE;
        }
    };
    coordinator.spawn_initial();
    let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(16);
    let server = tokio::spawn(stargazer::control::serve_socket(listener, request_tx));
    tracing::info!(socket = %socket_path.display(), "control socket ready (`stargazer ctl status`)");

    loop {
        tokio::select! {
            sig = tokio::signal::ctrl_c() => {
                match sig {
                    Ok(()) => tracing::info!("shutdown requested"),
                    Err(e) => tracing::error!(error = %e, "ctrl-c handler failed — shutting down"),
                }
                break;
            }
            () = terminate_signal() => {
                tracing::info!("termination requested (service stop)");
                break;
            }
            request = request_rx.recv() => {
                let Some((command, reply)) = request else { break };
                let response = coordinator.handle(command).await;
                let _unused = reply.send(response);
            }
        }
    }

    server.abort();
    if tokio::time::timeout(Duration::from_secs(8), coordinator.shutdown_all())
        .await
        .is_err()
    {
        tracing::warn!("supervisors did not drain within 8s — exiting anyway");
    }
    let _unused = std::fs::remove_file(&socket_path);
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
    options: stargazer::harvest::HarvestOptions,
) -> ExitCode {
    use stargazer::harvest::{Harvester, split_target};

    let date = date.unwrap_or_else(|| chrono::Utc::now().date_naive());
    let has_base_override = options.base_url.is_some();
    let harvester = match Harvester::new(options) {
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
                    let derivable = has_base_override
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

/// Resolves when SIGTERM arrives (how launchd stops a service);
/// pends forever where SIGTERM does not exist or cannot be hooked.
async fn terminate_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                let _unused = sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler failed — only ctrl-c will stop us");
                std::future::pending::<()>().await;
            }
        }
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await;
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
