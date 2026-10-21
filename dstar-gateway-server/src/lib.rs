#![doc = include_str!("../README.md")]

pub use client_pool::{
    ClientSnapshot, DEFAULT_TX_BUDGET_MAX_TOKENS, DEFAULT_TX_BUDGET_REFILL_PER_SEC, FrameRate,
    FrameRateError, TokenBucket,
};
pub use reflector::{
    AccessPolicy, AllowAllAuthorizer, ClientAuthorizer, ConfigDuration, ConfigError,
    DenyAllAuthorizer, LinkAttempt, ProtocolSockets, ReadOnlyAuthorizer, Reflector,
    ReflectorConfig, ReflectorConfigBuilder, RejectReason, STANDARD_DCS_PORT, STANDARD_DEXTRA_PORT,
    STANDARD_DPLUS_PORT, ShutdownHandle, StreamCache, StreamSource,
};
pub use tokio_shell::{
    CrossProtocolEvent, EndpointOutcome, ProtocolEndpoint, ShellError, VoiceEvent, VoiceRoute,
    transcode_voice,
};

// `trybuild` is consumed by the compile-fail runner under `tests/`,
// which is a separate compilation unit from this lib. Acknowledge it
// here so the dev-dep lint pass stays silent in this crate.
#[cfg(test)]
use trybuild as _;
// `tracing-subscriber` is consumed by the `polaris` binary and the
// `ref_reflector` example (separate compilation units). Acknowledge
// here so the lib's `unused-crate-dependencies` lint stays silent.
use tracing_subscriber as _;

mod client_pool;
pub mod reflector;
pub mod tokio_shell;
