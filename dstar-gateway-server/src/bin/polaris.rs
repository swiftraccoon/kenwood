// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! POLARIS: local D-STAR test reflector.
//!
//! Named after the navigational star that sits fixed while everything
//! else rotates around it: a single bind-point other clients can orient
//! toward. Binds a `DExtra` endpoint on `0.0.0.0:30001` by default,
//! installs [`AllowAllAuthorizer`], and runs until Ctrl-C.
//!
//! Run:
//!
//! ```text
//! cargo run -p dstar-gateway-server --bin polaris
//! ```
//!
//! Environment overrides:
//!
//! - `POLARIS_BIND`: bind address (default `0.0.0.0:30001`)
//! - `POLARIS_CALLSIGN`: reflector callsign (default `POLARIS`)
//! - `POLARIS_MODULES`: enabled module letters concatenated (default `ABCD`)
//! - `RUST_LOG`: tracing filter (unset falls back to [`DEFAULT_FILTER`]:
//!   `debug` for this crate and `dstar-gateway`, `info` for everything else)
//!
//! Enables only `DExtra`; `DPlus` and `Dcs` are disabled because a
//! local test reflector is simpler to exercise over the lightweight
//! `DExtra` handshake (no TCP auth step).

use std::collections::HashSet;
use std::env;

use dstar_gateway_core::types::{Module, ProtocolKind, RoutingCallsign};
use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig, ShellError};
use tracing_subscriber::EnvFilter;

// Workspace deps used elsewhere in the lib but not by this binary.
#[cfg(test)]
use trybuild as _;

/// Default tracing filter if `RUST_LOG` is not set. Aimed at
/// post-mortem debugging: `debug` for the workspace crates, `info` for
/// everything else. Override via `RUST_LOG=…` to crank up noise
/// during a specific diagnosis.
const DEFAULT_FILTER: &str = "dstar_gateway=debug,dstar_gateway_server=debug,polaris=debug,info";

/// Signal registration and reflector shutdown failures, preserving both causes.
#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("reflector failed: {0}")]
    Reflector(#[from] ShellError),
    #[error("Ctrl-C handler failed: {0}")]
    Signal(#[source] std::io::Error),
    #[error("Ctrl-C handler failed ({signal}); reflector shutdown failed ({reflector})")]
    SignalAndReflector {
        signal: std::io::Error,
        reflector: ShellError,
    },
}

fn environment_value(name: &str, fallback: &str) -> Result<String, env::VarError> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(fallback.to_owned()),
        Err(error) => Err(error),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER)),
        )
        .init();

    let bind_str = environment_value("POLARIS_BIND", "0.0.0.0:30001")?;
    let callsign_str = environment_value("POLARIS_CALLSIGN", "POLARIS")?;
    let modules_str = environment_value("POLARIS_MODULES", "ABCD")?;

    let mut modules = HashSet::new();
    for ch in modules_str.chars() {
        let _ = modules.insert(Module::try_from_char(ch)?);
    }

    let config = ReflectorConfig::builder()
        .callsign(RoutingCallsign::new(&callsign_str)?)
        .module_set(modules)
        .bind(bind_str.parse()?)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .max_clients_per_module(50)
        .build()?;

    tracing::info!(
        callsign = %callsign_str,
        bind = %bind_str,
        modules = %modules_str,
        "POLARIS reflector starting"
    );

    let reflector = Reflector::new(config, AllowAllAuthorizer);
    let shutdown = reflector.shutdown_handle();
    let run = reflector.run();
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => result.map_err(RunError::Reflector)?,
        signal = tokio::signal::ctrl_c() => {
            shutdown.shutdown();
            match (signal, run.await) {
                (Ok(()), Ok(())) => {}
                (Ok(()), Err(error)) => return Err(RunError::Reflector(error).into()),
                (Err(error), Ok(())) => return Err(RunError::Signal(error).into()),
                (Err(signal), Err(reflector)) => return Err(RunError::SignalAndReflector { signal, reflector }.into()),
            }
        }
    }

    Ok(())
}
