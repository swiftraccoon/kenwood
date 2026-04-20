// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Sextant — desktop D-STAR reflector client.
//!
//! Cross-platform egui app that lets the user connect to a D-STAR
//! reflector (`DExtra` / `DPlus` / `DCS`), receive voice from other clients
//! through the default speaker, and transmit via the default mic —
//! all with no radio in the loop. The primary use case is exercising
//! the local POLARIS test reflector end-to-end during development.
//!
//! ## Why "sextant"
//!
//! A sextant measures your position by angle-to-Polaris. Once you have
//! the POLARIS reflector running, `sextant` is the instrument you use
//! to talk to it.

#![windows_subsystem = "windows"]

mod app;
mod audio;
mod session;

use std::env;
use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _log_guard = init_logging();

    // Build a multi-thread tokio runtime so the session task and any
    // future IO can run concurrently. Keeping the runtime owned by
    // `main` means it lives for the full lifetime of the app.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Channels between the GUI thread and the async session task.
    // `cmd` flows GUI -> session (connect, disconnect, start/stop TX).
    // `evt` flows session -> GUI (status changes, log lines, RX audio
    // frames). Bounded capacities keep memory usage predictable; GUI
    // updates are lossless because `evt` is drained every redraw.
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(32);
    let (evt_tx, evt_rx) = tokio::sync::mpsc::channel(1024);

    // Build the audio worker BEFORE the session so we can hand its
    // `AudioHandle` down into the session loop.  The session task
    // routes incoming voice frames straight to the audio worker
    // (bypassing the GUI) so decode + playback aren't gated by the
    // egui redraw cadence (~50 ms → would mangle the 50 fps voice
    // stream otherwise).
    let audio = audio::AudioHandle::start(cmd_tx.clone());
    let audio_for_session = audio.clone();
    let _session_handle = runtime.spawn(session::run(cmd_rx, evt_tx, audio_for_session));

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sextant — D-STAR Client")
            .with_inner_size([720.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "sextant",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, cmd_tx, evt_rx, audio, runtime)))),
    )
    .map_err(|e| format!("eframe error: {e}").into())
}

/// Set up tracing: always-on file sink (`~/Library/Logs/sextant/` on
/// macOS, XDG state dir on Linux, `%LOCALAPPDATA%\sextant\logs` on
/// Windows), per-session filename at UTC second granularity.  stderr
/// mirror when `RUST_LOG` is set.
///
/// Default level = `debug` on the file sink so post-mortem debugging
/// of "my TX died at 1.29s" or "RX sounded glitched" has enough
/// context to diagnose from the log alone.  Override with `RUST_LOG`
/// (e.g. `RUST_LOG=sextant=trace,dstar_gateway=trace`).
fn init_logging() -> Option<WorkerGuard> {
    const DEFAULT_FILTER: &str = "sextant=debug,dstar_gateway=debug,dstar_gateway_core=info,warn";

    // Resolve the log directory and prepare a per-session filename.
    let dir = log_directory();
    let mut file_layer = None;
    let mut worker_guard = None;
    if let Some(dir) = dir.as_ref()
        && std::fs::create_dir_all(dir).is_ok()
    {
        let suffix = time::OffsetDateTime::now_utc()
            .format(time::macros::format_description!(
                "[year]-[month]-[day]-[hour][minute][second]"
            ))
            .unwrap_or_else(|_| "session".into());
        let path = dir.join(format!("sextant.log.{suffix}"));
        match std::fs::File::create(&path) {
            Ok(file) => {
                let (writer, guard) = tracing_appender::non_blocking(file);
                let filter = env::var("RUST_LOG").unwrap_or_else(|_| DEFAULT_FILTER.into());
                file_layer = Some(
                    fmt::layer()
                        .with_writer(writer)
                        .with_ansi(false)
                        .with_target(true)
                        .with_filter(EnvFilter::new(filter)),
                );
                worker_guard = Some(guard);
                eprintln!("sextant: logging to {}", path.display());
            }
            Err(e) => {
                eprintln!("sextant: could not create log file {}: {e}", path.display());
            }
        }
    }

    // stderr is opt-in via RUST_LOG.
    let stderr_layer = env::var("RUST_LOG").ok().map(|spec| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_filter(EnvFilter::new(spec))
    });

    Registry::default()
        .with(file_layer)
        .with(stderr_layer)
        .init();

    worker_guard
}

fn log_directory() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut path = dirs_next::home_dir()?;
        path.push("Library");
        path.push("Logs");
        path.push("sextant");
        Some(path)
    }
    #[cfg(target_os = "windows")]
    {
        let mut path = dirs_next::data_local_dir()?;
        path.push("sextant");
        path.push("logs");
        Some(path)
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
            let mut path = PathBuf::from(xdg);
            path.push("sextant");
            return Some(path);
        }
        let mut path = dirs_next::home_dir()?;
        path.push(".local");
        path.push("state");
        path.push("sextant");
        Some(path)
    }
}
