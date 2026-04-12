//! Dongle-only reflector: callsigns starting with `D` get read-only.
//!
//! A toy example of a policy authorizer that splits clients into
//! two cohorts:
//!
//! - Clients whose callsign starts with `D` are presumed to be
//!   dongle/hotspot users and are accepted with
//!   [`AccessPolicy::ReadOnly`] — they can receive every voice
//!   stream on the module, but their own transmissions are dropped
//!   at the fan-out layer.
//! - Everyone else gets [`AccessPolicy::ReadWrite`].
//!
//! The dongle rule is deliberately simplistic — a real deployment
//! would key on peer IP, registered station type, or an explicit
//! opt-in database rather than a callsign prefix. The example's
//! point is to show the `AccessPolicy::ReadOnly` wiring end-to-end.
//!
//! ```text
//! cargo run -p dstar-gateway-server --example 03_listen_only_dongles
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind};
use dstar_gateway_server::{
    AccessPolicy, ClientAuthorizer, LinkAttempt, Reflector, ReflectorConfig, RejectReason,
};

// Acknowledged workspace dev-deps.
use proptest as _;
use thiserror as _;
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

/// Authorizer that grants read-only access to callsigns starting
/// with `D` and read-write access to everyone else.
#[derive(Debug, Default, Clone, Copy)]
struct DongleOnlyAuthorizer;

impl ClientAuthorizer for DongleOnlyAuthorizer {
    fn authorize(&self, request: &LinkAttempt) -> Result<AccessPolicy, RejectReason> {
        // `Callsign::as_str` returns the 8-byte space-padded form
        // through a temporary; bind it to a local so the trimmed
        // view outlives the borrow check window.
        let callsign_str = request.callsign.as_str();
        let trimmed = callsign_str.trim();
        if trimmed.starts_with('D') {
            // Listen-only. The fan-out engine drops inbound voice
            // from this client but still forwards every outbound
            // stream from other clients in the module to it.
            Ok(AccessPolicy::ReadOnly)
        } else {
            Ok(AccessPolicy::ReadWrite)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // One module ('C') is enough for the demo; add more as needed.
    let mut modules = HashSet::new();
    let _ = modules.insert(Module::try_from_char('C')?);

    let config = ReflectorConfig::builder()
        .callsign(Callsign::try_from_str("REF999")?)
        .module_set(modules)
        .bind("0.0.0.0:30001".parse()?)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .max_clients_per_module(100)
        .build()?;

    let reflector = Arc::new(Reflector::new(config, DongleOnlyAuthorizer));
    reflector.run().await?;
    Ok(())
}
