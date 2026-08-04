//! Session-level link resilience.
//!
//! Home of the reconnect backoff policy shared by the D-STAR gateway
//! and [`RadioLinkRecovery`], the explicit wrapper that recovers a dropped
//! link by driving
//! [`Radio::reconnect`](crate::radio::Radio::reconnect) with backoff
//! while broadcasting typed [`LinkEvent`]s. [`ReconnectPolicy`] validates the
//! delay range, and [`ReconnectAttemptLimit`] makes a zero-attempt recovery run
//! impossible to construct.

mod policy;
mod recovery;

pub use policy::{ReconnectPolicy, ReconnectPolicyError};
pub use recovery::{
    LinkEvent, RadioLinkRecovery, ReconnectAttemptLimit, ReconnectAttemptLimitError,
};
