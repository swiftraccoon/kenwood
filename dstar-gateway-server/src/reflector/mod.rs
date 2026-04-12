//! Top-level reflector types: config, authorizer, stream cache, and
//! the [`Reflector`] front-end that owns all enabled endpoints.

pub mod authorizer;
pub mod config;
#[expect(
    clippy::module_inception,
    reason = "reflector::Reflector is the canonical naming — module_inception is too aggressive here"
)]
pub mod reflector;
pub mod stream_cache;

pub use authorizer::{
    AccessPolicy, AllowAllAuthorizer, ClientAuthorizer, DenyAllAuthorizer, LinkAttempt,
    ReadOnlyAuthorizer, RejectReason,
};
pub use config::{ConfigError, ReflectorConfig, ReflectorConfigBuilder};
pub use reflector::Reflector;
pub use stream_cache::StreamCache;
