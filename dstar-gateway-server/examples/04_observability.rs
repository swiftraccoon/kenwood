//! Production-grade structured logging for a reflector.
//!
//! Installs a `tracing-subscriber` with the JSON formatter so every
//! log line is a parsable JSON object suitable for ingest by
//! Loki / Datadog / Elasticsearch / etc. The reflector then runs
//! with [`AllowAllAuthorizer`] — in a real deployment this would be
//! a policy-aware authorizer similar to `02_authorized_reflector.rs`.
//!
//! Demonstrates the two separate knobs you generally want in a
//! production build:
//!
//! 1. A structured-log formatter (`tracing_subscriber::fmt::Layer::json`).
//! 2. An env-driven filter (`tracing_subscriber::EnvFilter::from_default_env`)
//!    so operators can tune verbosity without a rebuild.
//!
//! ```text
//! RUST_LOG=info,dstar_gateway_server=debug \
//!     cargo run -p dstar-gateway-server --example 04_observability
//! ```
//!
//! Notes:
//! - The JSON format is stable across tracing-subscriber releases.
//! - `tracing-subscriber` is a dev-dependency of this crate, so the
//!   example links against it without pulling it into downstream
//!   consumers.

use std::collections::HashSet;
use std::sync::Arc;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

// Acknowledged workspace dev-deps.
use proptest as _;
use thiserror as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a JSON-formatted subscriber with an env-driven filter.
    // The `.try_init()` call installs it as the process-wide
    // default; if something already installed one, we let that win.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json_layer = fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true);
    if let Err(e) = tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .try_init()
    {
        eprintln!("tracing subscriber already installed: {e}");
    }

    tracing::info!("starting observability-wired reflector");

    let mut modules = HashSet::new();
    let _ = modules.insert(Module::try_from_char('C')?);

    let config = ReflectorConfig::builder()
        .callsign(Callsign::try_from_str("REF999")?)
        .module_set(modules)
        .bind("0.0.0.0:30001".parse()?)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .build()?;

    let reflector = Arc::new(Reflector::new(config, AllowAllAuthorizer));
    reflector.run().await?;
    tracing::info!("reflector shut down cleanly");
    Ok(())
}
