//! Top-level reflector types: config, authorizer, stream cache, and
//! the [`Reflector`] front-end that owns all enabled endpoints.

pub use authorizer::{
    AccessPolicy, AllowAllAuthorizer, ClientAuthorizer, DenyAllAuthorizer, LinkAttempt,
    ReadOnlyAuthorizer, RejectReason,
};
pub use config::{
    ConfigDuration, ConfigError, ReflectorConfig, ReflectorConfigBuilder, STANDARD_DCS_PORT,
    STANDARD_DEXTRA_PORT, STANDARD_DPLUS_PORT,
};
pub use reflector::{ProtocolSockets, Reflector, ShutdownHandle};
pub use stream_cache::{StreamCache, StreamSource};

pub mod authorizer;
pub mod config;
#[expect(
    clippy::module_inception,
    reason = "reflector::Reflector is the canonical naming; module_inception is too aggressive here"
)]
pub mod reflector;
pub mod stream_cache;
