//! Minimal D-STAR reflector example (`DExtra` only).
//!
//! Binds a single `DExtra` endpoint on `0.0.0.0:30001`, installs
//! [`AllowAllAuthorizer`], and runs forever. This example is only
//! built (`cargo build --example ref_reflector -p dstar-gateway-server`);
//! CI does not execute it because it binds a real UDP port.
//!
//! ```text
//! cargo run -p dstar-gateway-server --example ref_reflector
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
use dstar_gateway_server::{AllowAllAuthorizer, Reflector, ReflectorConfig};

// Examples are a separate compilation unit — acknowledge workspace
// dev-deps we don't reference directly so the strict
// `unused_crate_dependencies` lint stays silent.
use proptest as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let mut modules = HashSet::new();
    let _ = modules.insert(Module::try_from_char('A')?);
    let _ = modules.insert(Module::try_from_char('B')?);
    let _ = modules.insert(Module::try_from_char('C')?);
    let _ = modules.insert(Module::try_from_char('D')?);

    let config = ReflectorConfig::builder()
        .callsign(Callsign::try_from_str("REF999")?)
        .module_set(modules)
        .bind("0.0.0.0:30001".parse()?)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .max_clients_per_module(50)
        .build()?;

    let reflector = Arc::new(Reflector::new(config, AllowAllAuthorizer));
    reflector.run().await?;
    Ok(())
}
