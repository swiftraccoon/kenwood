//! Reflector with a custom [`ClientAuthorizer`] that enforces a banlist.
//!
//! Demonstrates the shape of a production authorizer:
//! - Loads a banlist from the `BANLIST_FILE` env var (one callsign
//!   per line), falling back to a small hardcoded set when the file
//!   is missing or unreadable.
//! - Rejects any linking client whose callsign is on the list with
//!   [`RejectReason::Banned`].
//! - Accepts everyone else with [`AccessPolicy::ReadWrite`].
//!
//! The example compiles hermetically — the only I/O at runtime is
//! the reflector's own UDP bind, which happens inside `run()` and is
//! not triggered by `cargo build --example`.
//!
//! ```text
//! BANLIST_FILE=./banlist.txt cargo run -p dstar-gateway-server \
//!     --example 02_authorized_reflector
//! ```

use std::collections::HashSet;
use std::env;
use std::fs;
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

/// A simple banlist authorizer.
///
/// Stores banned callsigns in a [`HashSet`] so lookups are O(1).
/// Construction is infallible — an I/O failure while reading the
/// banlist file falls back to an empty set and logs a warning, so
/// the reflector still comes up.
#[derive(Debug, Default, Clone)]
struct BanlistAuthorizer {
    banned: HashSet<Callsign>,
}

impl BanlistAuthorizer {
    /// Build an authorizer from a file on disk.
    ///
    /// The file format is one callsign per line. Blank lines and
    /// comment lines starting with `#` are ignored. Unparsable
    /// callsigns are logged and skipped.
    fn from_file(path: &str) -> Self {
        let contents = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("could not read banlist at {path}: {e} — using empty list");
                return Self::default();
            }
        };
        let mut banned = HashSet::new();
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            match Callsign::try_from_str(trimmed) {
                Ok(cs) => {
                    let _ = banned.insert(cs);
                }
                Err(e) => {
                    eprintln!("skipping malformed banlist entry {trimmed:?}: {e}");
                }
            }
        }
        Self { banned }
    }

    /// Build an authorizer from an in-memory banlist, useful for
    /// tests and for the example's fallback when no file is set.
    fn from_callsigns(list: &[&str]) -> Self {
        let mut banned = HashSet::new();
        for entry in list {
            if let Ok(cs) = Callsign::try_from_str(entry) {
                let _ = banned.insert(cs);
            }
        }
        Self { banned }
    }
}

impl ClientAuthorizer for BanlistAuthorizer {
    fn authorize(&self, request: &LinkAttempt) -> Result<AccessPolicy, RejectReason> {
        if self.banned.contains(&request.callsign) {
            Err(RejectReason::Banned {
                reason: format!("{} is on the banlist", request.callsign.as_str().trim()),
            })
        } else {
            Ok(AccessPolicy::ReadWrite)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Build the authorizer from the env var, or fall back to a
    // small hardcoded list for the demo path.
    let authorizer = env::var("BANLIST_FILE").map_or_else(
        |_| BanlistAuthorizer::from_callsigns(&["BADACT1", "BADACT2"]),
        |path| BanlistAuthorizer::from_file(&path),
    );

    let mut modules = HashSet::new();
    let _ = modules.insert(Module::try_from_char('A')?);
    let _ = modules.insert(Module::try_from_char('B')?);
    let _ = modules.insert(Module::try_from_char('C')?);

    let config = ReflectorConfig::builder()
        .callsign(Callsign::try_from_str("REF999")?)
        .module_set(modules)
        .bind("0.0.0.0:30001".parse()?)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .max_clients_per_module(50)
        .build()?;

    let reflector = Arc::new(Reflector::new(config, authorizer));
    reflector.run().await?;
    Ok(())
}
