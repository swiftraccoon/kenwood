//! Reflector with a custom [`ClientAuthorizer`] that enforces a banlist.
//!
//! Demonstrates the shape of a production authorizer:
//! - Loads a banlist from the `BANLIST_FILE` env var (one callsign
//!   per line). When the variable is absent, a small hardcoded demo
//!   set is used. A configured file must be readable and every entry valid.
//! - Rejects any linking client whose callsign is on the list with
//!   [`RejectReason::Banned`].
//! - Accepts everyone else with [`AccessPolicy::ReadWrite`].
//!
//! Runtime I/O consists of the optional banlist read and reflector UDP service.
//! Building this example performs neither operation.
//!
//! ```text
//! BANLIST_FILE=./banlist.txt cargo run -p dstar-gateway-server \
//!     --example 02_authorized_reflector
//! ```

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, RoutingCallsign};
use dstar_gateway_server::{
    AccessPolicy, ClientAuthorizer, LinkAttempt, Reflector, ReflectorConfig, RejectReason,
};

// Acknowledged workspace dev-deps.
use tracing as _;
use tracing_subscriber as _;
use trybuild as _;

/// A simple banlist authorizer.
///
/// Stores banned callsigns in a [`HashSet`] so lookups are O(1).
/// A configured file must parse completely before the reflector starts.
#[derive(Debug, Default, Clone)]
struct BanlistAuthorizer {
    banned: HashSet<Callsign>,
}

#[derive(Debug, thiserror::Error)]
enum BanlistError {
    #[error("cannot read banlist: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid callsign on banlist line {line}: {source}")]
    Entry {
        line: usize,
        source: dstar_gateway_core::TypeError,
    },
}

impl BanlistAuthorizer {
    /// Build an authorizer from a file on disk.
    ///
    /// The file format is one callsign per line. Blank lines and
    /// comment lines starting with `#` are ignored. I/O or malformed entries
    /// return a typed error before any listening socket is opened.
    fn from_file(path: &Path) -> Result<Self, BanlistError> {
        Self::parse(&fs::read_to_string(path)?)
    }

    fn parse(contents: &str) -> Result<Self, BanlistError> {
        let mut banned = HashSet::new();
        for (index, line) in contents.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let callsign =
                Callsign::try_from_str(trimmed).map_err(|source| BanlistError::Entry {
                    line: index + 1,
                    source,
                })?;
            let _previous = banned.insert(callsign);
        }
        Ok(Self { banned })
    }
}

impl ClientAuthorizer for BanlistAuthorizer {
    fn authorize(&self, request: &LinkAttempt) -> Result<AccessPolicy, RejectReason> {
        if self.banned.contains(&request.callsign) {
            Err(RejectReason::Banned {
                reason: format!("{} is on the banlist", request.callsign),
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
    let authorizer = match env::var_os("BANLIST_FILE") {
        Some(path) => BanlistAuthorizer::from_file(Path::new(&path))?,
        None => BanlistAuthorizer::parse("BADACT1\nBADACT2\n")?,
    };

    let mut modules = HashSet::new();
    let _ = modules.insert(Module::try_from_char('A')?);
    let _ = modules.insert(Module::try_from_char('B')?);
    let _ = modules.insert(Module::try_from_char('C')?);

    let config = ReflectorConfig::builder()
        .callsign(RoutingCallsign::new("REF999")?)
        .module_set(modules)
        .bind("0.0.0.0:30001".parse()?)
        .disable(ProtocolKind::DPlus)
        .disable(ProtocolKind::Dcs)
        .max_clients_per_module(50)
        .build()?;

    let reflector = Reflector::new(config, authorizer);
    reflector.run().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_banlist_is_rejected_instead_of_skipping_policy() {
        let result = BanlistAuthorizer::parse("W1AW\nCALLSIGN-TOO-LONG\nK1ABC\n");
        assert!(
            matches!(result, Err(BanlistError::Entry { line: 2, .. })),
            "{result:?}"
        );
    }
}
