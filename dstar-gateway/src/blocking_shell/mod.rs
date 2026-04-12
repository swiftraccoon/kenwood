//! Synchronous (non-tokio) shell driving the sans-io core.
//!
//! Same typestate API as [`super::tokio_shell`], but uses
//! `std::net::UdpSocket` with read timeouts instead of tokio channels.
//!
//! Useful for CLI scripts, test fixtures, and documentation examples
//! that don't want to drag in a tokio runtime.
//!
//! Unlike the async [`super::tokio_shell::AsyncSession`], the blocking
//! shell is **caller-driven**: the consumer calls
//! [`BlockingSession::run_until_event`] in a loop to drive the driver
//! state machine forward one step at a time. No spawned thread, no
//! channels.

mod session;

pub use session::BlockingSession;
