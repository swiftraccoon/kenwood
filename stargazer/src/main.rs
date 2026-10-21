// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Stargazer CLI entry point.

use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(unix)]
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use dstar_gateway as _;
use dstar_gateway_core::RoutingCallsign;
use mbelib_rs as _;
use reqwest as _;
use serde as _;
use serde_json as _;
#[cfg(test)]
use tempfile as _;
use thiserror as _;
use toml as _;

use stargazer::config::{Config, TargetKey};
use stargazer::harvest::{DashboardUrl, HarvestOptions};
use stargazer::survey::{ActivityRanking, SurveyLimits};

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
        /// Operator callsign in the User-Agent; otherwise read from --config.
        #[arg(long)]
        operator: Option<RoutingCallsign>,
        /// Maximum retained distinct event identities; raise to resume a full archive.
        #[arg(long, default_value = "1000000")]
        max_seen: NonZeroUsize,
        /// Maximum response body bytes.
        #[arg(long, default_value = "1048576")]
        max_response_bytes: NonZeroUsize,
        /// Maximum JSON bytes per archived line.
        #[arg(long, default_value = "1048576")]
        max_line_bytes: NonZeroUsize,
    },
    /// Rank reflector modules by archived voice activity.
    Report {
        /// Archive directory (as used by `survey`).
        #[arg(long, default_value = "survey")]
        out: PathBuf,
        /// Ranking window in hours.
        #[arg(long, default_value_t = 24)]
        window_hours: u64,
        /// Maximum JSON bytes per archived line.
        #[arg(long, default_value = "1048576")]
        max_line_bytes: NonZeroUsize,
    },
    /// Fetch reflector-published recordings (MP3s,
    /// sidecars, gap-fill packet logs) that pair with local
    /// recordings, into per-date `published/` subdirectories.
    Harvest {
        /// Recordings directory (as written by the recorder).
        #[arg(long, default_value = "recordings")]
        recordings: PathBuf,
        /// Date to harvest, YYYY-MM-DD (UTC). Defaults to today.
        #[arg(long, value_parser = parse_date)]
        date: Option<chrono::NaiveDate>,
        /// Restrict to one reflector-module directory (repeatable,
        /// e.g. `--target REF030-C`). Default: every directory.
        #[arg(long)]
        target: Vec<TargetKey>,
        /// Dashboard base URL override (testing / non-REF systems).
        #[arg(long)]
        base_url: Option<DashboardUrl>,
        /// Maximum downloads this run.
        #[arg(long)]
        limit: Option<NonZeroUsize>,
        /// Match and report only; download and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Operator callsign advertised in the User-Agent so server
        /// owners can identify and contact who is fetching (default:
        /// the callsign from --config when that file exists).
        #[arg(long)]
        operator: Option<RoutingCallsign>,
    },
    /// Reconstruct recordings from published dvrec packet logs for
    /// transmissions with no locally captured twin (salvage from
    /// recorder-offline windows and unlinked reflectors).
    ImportDvrec {
        /// Recordings directory (as written by the recorder).
        #[arg(long, default_value = "recordings")]
        recordings: PathBuf,
        /// Maximum retained prefix per imported transmission, in seconds.
        #[arg(long, default_value = "3600")]
        max_capture_seconds: NonZeroU64,
        /// Maximum input bytes for one packet log.
        #[arg(long, default_value = "67108864")]
        max_input_bytes: NonZeroUsize,
    },
    /// Control local recorder sessions through a Unix socket.
    Ctl {
        #[command(subcommand)]
        action: CtlAction,
    },
    /// Export a recording's vocoder-parameter features (170 f32 per
    /// 20 ms frame, little-endian, gap-concealed) for codec-domain
    /// ASR training.
    Features {
        /// Input `.ambe` frame container.
        input: PathBuf,
        /// Output raw f32 file.
        out: PathBuf,
    },
}

/// Actions for `stargazer ctl`.
#[derive(Debug, Subcommand)]
enum CtlAction {
    /// Show every configured target's state.
    Status,
    /// Stop one local target and persist its disabled state.
    Disable {
        /// Target key, e.g. `REF030-C`.
        target: TargetKey,
    },
    /// Relink a disabled target.
    Enable {
        /// Target key, e.g. `REF030-C`.
        target: TargetKey,
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
            operator,
            max_seen,
            max_response_bytes,
            max_line_bytes,
        }) => {
            let operator = match resolve_operator(operator, &args.config) {
                Ok(operator) => operator,
                Err(error) => {
                    tracing::error!(%error, "set --operator or provide a valid --config");
                    return ExitCode::FAILURE;
                }
            };
            let limits = SurveyLimits::new(max_seen)
                .with_response_bytes(max_response_bytes)
                .with_line_bytes(max_line_bytes);
            survey(&out, interval, once, limits, operator).await
        }
        Some(Cmd::Report {
            out,
            window_hours,
            max_line_bytes,
        }) => report(&out, window_hours, max_line_bytes),
        Some(Cmd::Features { input, out }) => {
            match stargazer::features::export_file(&input, &out) {
                Ok(frames) => {
                    tracing::info!(frames, out = %out.display(), "features exported");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    tracing::error!(error = %e, "feature export failed");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Cmd::Harvest {
            recordings,
            date,
            target,
            base_url,
            limit,
            dry_run,
            operator,
        }) => {
            let operator = match resolve_operator(operator, &args.config) {
                Ok(operator) => operator,
                Err(error) => {
                    tracing::error!(%error, "set --operator or provide a valid --config");
                    return ExitCode::FAILURE;
                }
            };
            let mut options = HarvestOptions::new(operator).with_dry_run(dry_run);
            if let Some(base) = base_url {
                options = options.with_base_url(base);
            }
            if let Some(maximum) = limit {
                options = options.with_limit(maximum);
            }
            harvest(&recordings, date, &target, options).await
        }
        Some(Cmd::ImportDvrec {
            recordings,
            max_capture_seconds,
            max_input_bytes,
        }) => {
            let capture = match stargazer::capture::CaptureDurationLimit::try_from_seconds(
                max_capture_seconds.get(),
            ) {
                Ok(limit) => limit,
                Err(error) => {
                    tracing::error!(%error, "import capture limit is not representable");
                    return ExitCode::FAILURE;
                }
            };
            import_dvrec(
                &recordings,
                stargazer::dvrec::ImportLimits::new(max_input_bytes, capture),
            )
        }
        Some(Cmd::Ctl { action }) => ctl(&args.config, action).await,
    }
}

fn resolve_operator(
    explicit: Option<RoutingCallsign>,
    config_path: &std::path::Path,
) -> Result<RoutingCallsign, stargazer::config::ConfigError> {
    explicit.map_or_else(
        || Config::load(config_path).map(|config| config.callsign()),
        Ok,
    )
}

/// Import salvaged dvrec packet logs as recordings.
///
/// Per-file parse and write failures are logged and counted in the
/// printed summary but do not fail the run: corrupt published files
/// persist on disk indefinitely, so treating them as fatal would
/// permanently wedge any pipeline that runs the importer ahead of
/// later stages. The exit code reports only whether the recordings
/// tree itself could be walked.
fn import_dvrec(recordings: &std::path::Path, limits: stargazer::dvrec::ImportLimits) -> ExitCode {
    let writer = stargazer::writer::Writer::new(recordings.to_path_buf(), true);
    match stargazer::dvrec::import_tree(recordings, &writer, limits) {
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
            ExitCode::SUCCESS
        }
    }
}

/// Send one control command to the running recorder and print the
/// response.
#[cfg(unix)]
async fn ctl(config_path: &std::path::Path, action: CtlAction) -> ExitCode {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let config = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(path = %config_path.display(), error = %e, "config load failed");
            return ExitCode::FAILURE;
        }
    };
    let socket = config
        .recordings_dir()
        .join(stargazer::control::SOCKET_FILE);
    let line = match action {
        CtlAction::Status => "status".to_string(),
        CtlAction::Disable { target } => format!("disable {target}"),
        CtlAction::Enable { target } => format!("enable {target}"),
        CtlAction::Reload => "reload".to_string(),
    };
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(&socket).await?;
        stream.write_all(format!("{line}\n").as_bytes()).await?;
        let mut response = Vec::new();
        let _count = stream.take(1_048_577).read_to_end(&mut response).await?;
        if response.len() > 1_048_576 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "control response exceeds 1 MiB",
            ));
        }
        String::from_utf8(response)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    };
    match tokio::time::timeout(Duration::from_secs(10), exchange).await {
        Ok(Ok(response)) => {
            print!("{response}");
            ExitCode::SUCCESS
        }
        Ok(Err(error)) => {
            tracing::error!(%error, socket = %socket.display(), "control request failed; check recorder status");
            ExitCode::FAILURE
        }
        Err(_) => {
            tracing::error!(
                "control request exceeded 10 seconds; inspect recorder status before retrying"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn ctl(_config_path: &std::path::Path, _action: CtlAction) -> std::future::Ready<ExitCode> {
    tracing::error!("recorder control requires Unix sockets; use a Unix platform");
    std::future::ready(ExitCode::FAILURE)
}

/// Default mode: record configured reflector targets.
#[cfg(unix)]
async fn record(config_path: &std::path::Path) -> ExitCode {
    let config = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(path = %config_path.display(), error = %e, "config load failed");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::create_dir_all(config.recordings_dir()) {
        tracing::error!(
            dir = %config.recordings_dir().display(),
            error = %e,
            "cannot create recordings dir"
        );
        return ExitCode::FAILURE;
    }
    let _instance_lock = match claim_instance(config.recordings_dir()) {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, "cannot lock recorder directory; stop the other recorder or inspect permissions");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        targets = config.targets().len(),
        recordings_dir = %config.recordings_dir().display(),
        max_capture_seconds = config.capture_limit().as_seconds(),
        max_concurrent_captures = config.concurrent_capture_limit().get(),
        "stargazer recording"
    );

    let writer = Arc::new(stargazer::writer::Writer::new(
        config.recordings_dir().to_owned(),
        config.write_wav(),
    ));
    let mut coordinator =
        match stargazer::control::Coordinator::new(config, config_path.to_path_buf(), writer) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "coordinator init failed");
                return ExitCode::FAILURE;
            }
        };
    let socket_path = coordinator.socket_path();
    let listener = match bind_control_socket(&socket_path).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(socket = %socket_path.display(), error = %e, "control socket initialization failed");
            return ExitCode::FAILURE;
        }
    };
    coordinator.spawn_initial();
    let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(16);
    let server = tokio::spawn(stargazer::control::serve_socket(listener, request_tx));
    tracing::info!(socket = %socket_path.display(), "control socket ready (`stargazer ctl status`)");

    let mut reconcile = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            sig = tokio::signal::ctrl_c() => {
                match sig {
                    Ok(()) => tracing::info!("shutdown requested"),
                    Err(e) => tracing::error!(error = %e, "ctrl-c handler failed, shutting down"),
                }
                break;
            }
            () = terminate_signal() => {
                tracing::info!("termination requested (service stop)");
                break;
            }
            request = request_rx.recv() => {
                let Some(stargazer::control::CommandRequest { command, reply }) = request else { break };
                let response = coordinator.handle(command).await;
                log_completions(response.completions());
                let _delivered = reply.send(response.message().to_owned());
            }
            _tick = reconcile.tick() => log_completions(&coordinator.reconcile().await),
        }
    }

    drop(request_rx);
    let server_ok = match server.await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            tracing::error!(%error, "control server failed");
            false
        }
        Err(error) => {
            tracing::error!(%error, "control server task failed");
            false
        }
    };
    log_completions(&coordinator.shutdown_all().await);
    if let Err(error) = std::fs::remove_file(&socket_path) {
        tracing::error!(%error, "cannot remove recorder socket");
        return ExitCode::FAILURE;
    }
    if server_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(unix)]
fn claim_instance(directory: &std::path::Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".recorder.lock"))?;
    file.try_lock()?;
    Ok(file)
}

#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
enum ControlStartupError {
    #[error("cannot determine socket state: {0}; inspect it before starting")]
    Probe(#[from] stargazer::control::ProbeError),
    #[error("recorder socket is occupied; use the existing recorder")]
    Occupied,
    #[error("socket creation: {0}; inspect the path and permissions")]
    Io(#[from] std::io::Error),
}

#[cfg(unix)]
async fn bind_control_socket(
    path: &std::path::Path,
) -> Result<tokio::net::UnixListener, ControlStartupError> {
    if stargazer::control::recorder_already_running(path).await? {
        return Err(ControlStartupError::Occupied);
    }
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(error.into());
    }
    Ok(tokio::net::UnixListener::bind(path)?)
}

#[cfg(not(unix))]
fn record(_config_path: &std::path::Path) -> std::future::Ready<ExitCode> {
    tracing::error!("recording requires Unix socket coordination; use a Unix platform");
    std::future::ready(ExitCode::FAILURE)
}

#[cfg(unix)]
fn log_completions(completions: &[stargazer::control::TargetCompletion]) {
    for completion in completions {
        match &completion.result {
            Ok(summary) => {
                if let Some(error) = summary.last_failure() {
                    tracing::warn!(target = %completion.target, %error, written = summary.recordings_written(), failed_writes = summary.failed_writes(), "supervisor stopped with failures");
                }
            }
            Err(error) => {
                tracing::error!(target = %completion.target, %error, "supervisor task failed");
            }
        }
    }
}

/// Survey mode: poll the DPLUSMON feed gently and archive everything.
async fn survey(
    out: &std::path::Path,
    interval: u64,
    once: bool,
    limits: SurveyLimits,
    operator: RoutingCallsign,
) -> ExitCode {
    if interval < stargazer::survey::MIN_INTERVAL_SECS
        || Duration::from_secs(interval)
            .checked_mul(16)
            .and_then(|duration| tokio::time::Instant::now().checked_add(duration))
            .is_none()
    {
        tracing::error!(
            "poll interval must be at least 30 seconds and fit its 16-times backoff deadline"
        );
        return ExitCode::FAILURE;
    }
    let mut surveyor = match stargazer::survey::Surveyor::new(out, limits, operator) {
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
                // Archive I/O failure: the archive is the point.
                tracing::error!(error = %e, "survey archive failure");
                return ExitCode::FAILURE;
            }
            Ok(rec) => {
                if let Some(err) = rec.error() {
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
                            "feed window rolled over between polls; some rows may be missing"
                        );
                    }
                }
            }
        }
        if once {
            return ExitCode::SUCCESS;
        }
        // Back off politely on repeated errors: up to 16× interval.
        let factor = 1u32 << consecutive_errors.min(4);
        let Some(sleep) = Duration::from_secs(interval)
            .checked_mul(factor)
            .and_then(|duration| tokio::time::Instant::now().checked_add(duration))
        else {
            tracing::error!("next poll deadline cannot be represented");
            return ExitCode::FAILURE;
        };
        tokio::select! {
            () = tokio::time::sleep_until(sleep) => {}
            result = tokio::signal::ctrl_c() => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "ctrl-c handler failed, exiting");
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
    targets: &[TargetKey],
    options: HarvestOptions,
) -> ExitCode {
    use stargazer::harvest::Harvester;

    let date = date.unwrap_or_else(|| chrono::Utc::now().date_naive());
    let has_base_override = options.base_url().is_some();
    let mut harvester = match Harvester::new(options) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "harvester init failed");
            return ExitCode::FAILURE;
        }
    };

    let mut names = if targets.is_empty() {
        match discover_targets(recordings, has_base_override) {
            Ok(targets) => targets,
            Err(error) => {
                tracing::error!(%error, dir = %recordings.display(), "cannot read recordings directory");
                return ExitCode::FAILURE;
            }
        }
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

fn discover_targets(
    recordings: &std::path::Path,
    has_base_override: bool,
) -> std::io::Result<Vec<TargetKey>> {
    let mut targets = Vec::new();
    for entry in std::fs::read_dir(recordings)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(target) = name.parse::<TargetKey>() else {
            continue;
        };
        if has_base_override || DashboardUrl::for_reflector(target.reflector()).is_some() {
            targets.push(target);
        } else {
            tracing::info!(%target, "no default dashboard address; set --target and --base-url to fetch it");
        }
    }
    Ok(targets)
}

/// One human-readable coverage line per harvested target.
fn print_harvest_run(rec: &stargazer::harvest::RunRecord) {
    if let Some(err) = rec.error() {
        println!("{} {}: listing unavailable: {err}", rec.target, rec.date);
        return;
    }
    if rec.http_status == Some(404) {
        println!("{} {}: nothing published", rec.target, rec.date);
        return;
    }
    let pct = if rec.published_tx > 0 {
        rec.matched.saturating_mul(100) / rec.published_tx
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
        "{} {}: published {} · local {} · matched {} ({pct}%) · salvage {} · local-only {}; {action}",
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
fn report(out: &std::path::Path, window_hours: u64, maximum_line: NonZeroUsize) -> ExitCode {
    let archive = stargazer::survey::Archive::new(out);
    let events = match archive.events(maximum_line) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, archive = %out.display(), "cannot read archive");
            return ExitCode::FAILURE;
        }
    };
    let since = i64::try_from(window_hours)
        .ok()
        .and_then(chrono::TimeDelta::try_hours)
        .and_then(|window| chrono::Utc::now().checked_sub_signed(window));
    let Some(since) = since else {
        tracing::error!("ranking window exceeds the representable UTC range; choose fewer hours");
        return ExitCode::FAILURE;
    };
    let mut ranking = ActivityRanking::new(since);
    let mut count = 0usize;
    for event in events {
        match event {
            Ok(event) => {
                ranking.observe(&event);
                count = count.saturating_add(1);
            }
            Err(error) => {
                tracing::error!(%error, "invalid activity archive; inspect the reported line");
                return ExitCode::FAILURE;
            }
        }
    }
    let ranked = ranking.into_rows();

    println!("DPLUS voice activity: last {window_hours}h ({count} events archived)");
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
        println!("(no reflector activity in window; is the survey running?)");
    }
    ExitCode::SUCCESS
}

/// Resolves when SIGTERM arrives (how launchd stops a service);
/// pends forever where SIGTERM does not exist or cannot be hooked.
#[cfg(unix)]
async fn terminate_signal() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut sig) => {
            let _unused = sig.recv().await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "SIGTERM handler failed; only ctrl-c will stop us");
            std::future::pending::<()>().await;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn command_line_rejects_unsafe_targets_and_invalid_http_options() {
        for arguments in [
            vec!["stargazer", "harvest", "--target", "../..-C"],
            vec!["stargazer", "harvest", "--base-url", "file:///tmp/captures"],
            vec!["stargazer", "harvest", "--limit", "0"],
            vec!["stargazer", "survey", "--max-seen", "0"],
            vec!["stargazer", "survey", "--operator", "TOOLONGCALL"],
        ] {
            assert!(Args::try_parse_from(&arguments).is_err(), "{arguments:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn recorder_directory_lock_prevents_a_second_instance() -> TestResult {
        let directory = tempfile::tempdir()?;
        let first = claim_instance(directory.path())?;
        assert!(claim_instance(directory.path()).is_err());
        drop(first);
        let _replacement = claim_instance(directory.path())?;
        Ok(())
    }

    #[tokio::test]
    async fn unrepresentable_poll_and_report_windows_fail_before_network_io() -> TestResult {
        let directory = tempfile::tempdir()?;
        let limits = SurveyLimits::new(NonZeroUsize::new(100).ok_or("limit")?);
        for interval in [0, 29, u64::MAX] {
            assert_eq!(
                survey(
                    directory.path(),
                    interval,
                    true,
                    limits,
                    RoutingCallsign::new("N0CALL")?
                )
                .await,
                ExitCode::FAILURE
            );
        }
        assert_eq!(
            report(directory.path(), u64::MAX, limits.line_bytes()),
            ExitCode::FAILURE
        );
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }
}
