//! Stargazer: D-STAR network observatory.
//!
//! A headless Kubernetes-deployed service that discovers active D-STAR
//! reflectors, monitors real-time activity, captures voice transmissions with
//! metadata, decodes AMBE audio to MP3, and uploads completed streams to an
//! `SDRTrunk`-compatible Rdio API server for transcription.
//!
//! # Architecture
//!
//! Stargazer operates in three tiers:
//!
//! - **Tier 1** (Discovery): polls Pi-Star, XLX API, and ircDDB to build a
//!   reflector registry.
//! - **Tier 2** (Monitoring): connects to active XLX reflectors via UDP JSON
//!   monitor protocol for real-time activity events.
//! - **Tier 3** (Capture): establishes full D-STAR protocol connections to
//!   capture and decode voice streams.
//!
//! All tiers run as independent tokio tasks. A background upload processor
//! sends completed streams to the Rdio API server. An HTTP API provides
//! operational visibility and manual session control.
//!
//! # Usage
//!
//! ```text
//! stargazer --config stargazer.toml
//! ```

// Dependencies used by submodules but not yet referenced from main.rs directly.
// Each stub module will use these once implemented; the `use as _` suppresses
// the unused_crate_dependencies lint per file (not a blanket allow).
use dstar_gateway as _;
use dstar_gateway_core as _;
use mbelib_rs as _;
use mp3lame_encoder as _;
use quick_xml as _;
use reqwest as _;
use scraper as _;
use thiserror as _;

mod api;
mod config;
mod db;
mod tier1;
mod tier2;
mod tier3;
mod upload;

use std::path::PathBuf;

use clap::Parser;

/// D-STAR network observatory — reflector monitoring and voice capture service.
#[derive(Debug, Parser)]
#[command(name = "stargazer", version, about)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, default_value = "stargazer.toml")]
    config: PathBuf,
}

fn main() {
    let cli = Cli::parse();

    // Initialize structured JSON logging with env-filter support.
    // Use `RUST_LOG=stargazer=debug` to control verbosity.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("stargazer=info")),
        )
        .init();

    let config = match config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(path = %cli.config.display(), error = %e, "failed to load config");
            std::process::exit(1);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to build tokio runtime");
            std::process::exit(1);
        }
    };

    runtime.block_on(run(config));
}

/// Top-level async entry point.
///
/// Connects to Postgres, runs migrations, spawns all tier orchestrators
/// and the upload processor, starts the HTTP API, then waits for a
/// shutdown signal (SIGTERM / ctrl-c).
///
/// Each tier runs as an independent tokio task — a crash in Tier 2
/// does not affect Tier 3. On shutdown, all tasks are aborted
/// (graceful drain will be refined later).
async fn run(config: config::Config) {
    tracing::info!(
        postgres_url = %config.postgres.url,
        tier1_pistar = config.tier1.pistar,
        tier2_max_monitors = config.tier2.max_concurrent_monitors,
        tier3_max_connections = config.tier3.max_concurrent_connections,
        server_listen = %config.server.listen,
        "stargazer starting"
    );

    // Connect to Postgres and run schema migrations.
    let pool = match db::connect(&config.postgres).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to postgres");
            return;
        }
    };
    if let Err(e) = db::migrate(&pool).await {
        tracing::error!(error = %e, "failed to run migrations");
        return;
    }
    tracing::info!("database connected and migrated");

    // Spawn each tier as an independent tokio task so that a failure
    // in one does not take down the others.
    let api_handle = tokio::spawn(api::serve(config.server.listen, pool.clone()));
    let t1_handle = tokio::spawn(tier1::run(config.tier1, pool.clone()));
    let t2_handle = tokio::spawn(tier2::run(config.tier2, pool.clone()));
    let t3_handle = tokio::spawn(tier3::run(config.tier3, config.audio, pool.clone()));
    let upload_handle = tokio::spawn(upload::run(config.rdio, pool.clone()));

    tracing::info!("all tiers started");

    // Wait for shutdown signal.
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("received ctrl-c, shutting down"),
        Err(e) => tracing::error!(error = %e, "failed to listen for ctrl-c"),
    }

    // Abort all tasks. Graceful shutdown (flush pending writes,
    // disconnect sessions, drain upload queue) will be added later.
    api_handle.abort();
    t1_handle.abort();
    t2_handle.abort();
    t3_handle.abort();
    upload_handle.abort();
}
