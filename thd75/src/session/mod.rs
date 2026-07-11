//! Session-level link resilience.
//!
//! Home of the reconnect backoff policy shared by the D-STAR gateway
//! and the radio supervisor, and of [`RadioSupervisor`] — the opt-in
//! wrapper that heals a dropped link by driving
//! [`Radio::reconnect`](crate::radio::Radio::reconnect) with backoff
//! while broadcasting typed [`LinkEvent`]s.

mod policy;
mod supervisor;

pub use policy::ReconnectPolicy;
pub use supervisor::{LinkEvent, RadioSupervisor};
