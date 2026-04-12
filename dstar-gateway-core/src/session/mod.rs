//! Sans-io session machinery.
//!
//! The `Driver` trait is the contract every session implements.
//! See [`driver`] for the trait definition. Per-protocol client
//! sessions live in [`client`]. Server sessions live in [`server`]
//! (currently DExtra-only).

pub mod client;
pub mod driver;
pub mod outbox;
pub mod server;
pub mod timer_wheel;

pub use driver::{Driver, Transmit};
