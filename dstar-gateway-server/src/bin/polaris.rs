// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! POLARIS — local D-STAR test reflector.
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
//! - `POLARIS_BIND` — bind address (default `0.0.0.0:30001`)
//! - `POLARIS_CALLSIGN` — reflector callsign (default `POLARIS`)
//! - `POLARIS_MODULES` — enabled module letters concatenated (default `ABCD`)
//! - `RUST_LOG` — tracing filter (default `info`)
//!
//! Enables only `DExtra`; `DPlus` and `Dcs` are disabled because a
//! local test reflector is simpler to exercise over the lightweight
//! `DExtra` handshake (no TCP auth step).

use std::collections::HashSet;
use std::env;
use std::sync::Arc;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig};
use tracing_subscriber::EnvFilter;

// Workspace deps used elsewhere in the lib but not by this binary.
#[cfg(test)]
use proptest as _;
use thiserror as _;
#[cfg(test)]
use trybuild as _;

/// Default tracing filter if `RUST_LOG` is not set. Aimed at
/// post-mortem debugging: `debug` for our own crates, `info` for
/// everything else. Override via `RUST_LOG=…` to crank up noise
/// during a specific diagnosis.
const DEFAULT_FILTER: &str = "dstar_gateway=debug,dstar_gateway_server=debug,polaris=debug,info";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER)),
        )
        .init();

    let bind_str = env::var("POLARIS_BIND").unwrap_or_else(|_| "0.0.0.0:30001".to_string());
    let callsign_str = env::var("POLARIS_CALLSIGN").unwrap_or_else(|_| "POLARIS".to_string());
    let modules_str = env::var("POLARIS_MODULES").unwrap_or_else(|_| "ABCD".to_string());

    let mut modules = HashSet::new();
    for ch in modules_str.chars() {
        let _ = modules.insert(Module::try_from_char(ch)?);
    }

    let config = ReflectorConfig::builder()
        .callsign(Callsign::try_from_str(&callsign_str)?)
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

    let reflector = Arc::new(Reflector::new(config, AllowAllAuthorizer));
    let reflector_clone = Arc::clone(&reflector);

    let run_task = tokio::spawn(async move { reflector_clone.run().await });

    tokio::select! {
        res = run_task => {
            match res {
                Ok(Ok(())) => tracing::info!("reflector exited cleanly"),
                Ok(Err(e)) => tracing::error!(error = %e, "reflector error"),
                Err(e) => tracing::error!(error = %e, "reflector task join error"),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Ctrl-C received, shutting down");
        }
    }

    Ok(())
}
