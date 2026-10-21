//! `ReflectorConfig::builder().build()` must not compile without a
//! callsign. The typestate builder only implements `.build()` on
//! `ReflectorConfigBuilder<Provided, Provided, Provided>`, so
//! omitting the callsign leaves the marker `Cs = Missing` and no
//! matching `build` method exists.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use dstar_gateway_server::ReflectorConfig;

fn main() {
    // Missing callsign: only 2 of 3 required fields set.
    let builder = ReflectorConfig::builder()
        .module_set(HashSet::new())
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));

    // ERROR: no method named `build` found for
    // `ReflectorConfigBuilder<Missing, Provided, Provided>`.
    let _config = builder.build();
}
