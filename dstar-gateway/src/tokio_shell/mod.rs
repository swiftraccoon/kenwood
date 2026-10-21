//! Tokio async shell driving the sans-io `dstar-gateway-core`.
//!
//! This module is the async API over the sans-io core.
//!
//! Entry points:
//! - [`ShellError`]: shell-level errors (wraps core `Error` + adds
//!   channel/task-closed variants)
//! - [`AsyncSession`]: user-facing handle over a spawned session;
//!   use [`AsyncSession::spawn`] to wire up the internal session loop
//!
//! The internal `SessionLoop` type is crate-private; it's constructed
//! by [`AsyncSession::spawn`] and should not be touched directly by
//! consumers.

pub use any_session::{AnyAsyncSession, AnyEvent};
pub(crate) use command::Command;
pub use connect::{ConnectError, drive_connecting};
pub use error::ShellError;
pub use handle::AsyncSession;
pub(crate) use session_loop::SessionLoop;
pub use stream_id::fresh_stream_id;

mod any_session;
mod command;
mod connect;
mod error;
mod handle;
mod session_loop;
mod stream_id;
