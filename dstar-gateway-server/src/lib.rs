//! Multi-client D-STAR reflector server.
//!
//! Supports all three reflector protocols — `DExtra`, `DPlus`, and
//! `DCS` — behind a common [`Reflector`] front-end. Each enabled
//! endpoint runs on its own tokio task and (when
//! `cross_protocol_forwarding = true` in the config) publishes to a
//! shared broadcast bus so voice frames received on one protocol
//! are transcoded and re-broadcast on the other two.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::unreachable,
    )
)]

pub mod client_pool;
pub mod reflector;
pub mod tokio_shell;

pub use client_pool::{
    ClientHandle, ClientPool, DEFAULT_TX_BUDGET_MAX_TOKENS, DEFAULT_TX_BUDGET_REFILL_PER_SEC,
    DEFAULT_UNHEALTHY_THRESHOLD, TokenBucket, UnhealthyOutcome,
};
pub use reflector::{
    AccessPolicy, AllowAllAuthorizer, ClientAuthorizer, ConfigError, DenyAllAuthorizer,
    LinkAttempt, ReadOnlyAuthorizer, Reflector, ReflectorConfig, ReflectorConfigBuilder,
    RejectReason, StreamCache,
};
pub use tokio_shell::{
    CrossProtocolEvent, EndpointOutcome, FanOutReport, ProtocolEndpoint, ShellError, VoiceEvent,
    fan_out_voice, fan_out_voice_at, transcode_voice,
};

// `proptest` is a dev-dependency used by future property tests.
// Acknowledge it so `-D unused-crate-dependencies` stays quiet until
// the test file lands.
#[cfg(test)]
use proptest as _;
// `trybuild` is consumed by the compile-fail runner under `tests/`,
// which is a separate compilation unit from this lib. Acknowledge it
// here so the dev-dep lint pass stays silent in this crate.
#[cfg(test)]
use trybuild as _;
// `tracing-subscriber` is consumed by the `ref_reflector` example
// (separate compilation unit). Acknowledge it here so the lib test
// crate's dev-dep lint pass stays silent.
#[cfg(test)]
use tracing_subscriber as _;
