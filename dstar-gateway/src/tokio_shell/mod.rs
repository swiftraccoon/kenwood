//! Tokio async shell driving the sans-io `dstar-gateway-core`.
//!
//! This module provides the async API consumers will use once the
//! legacy `ReflectorClient` is retired. For now it lives alongside
//! the legacy code.
//!
//! Entry points:
//! - [`Command`] — messages sent from the [`AsyncSession`] handle
//!   to the spawned session task
//! - [`ShellError`] — shell-level errors (wraps core `Error` + adds
//!   channel/task-closed variants)
//! - [`AsyncSession`] — user-facing handle over a spawned session;
//!   use [`AsyncSession::spawn`] to wire up the internal session loop
//!
//! The internal `SessionLoop` type is crate-private — it's constructed
//! by [`AsyncSession::spawn`] and should not be touched directly by
//! consumers.

mod command;
mod error;
mod handle;
mod session_loop;

pub use command::Command;
pub use error::ShellError;
pub use handle::AsyncSession;
pub(crate) use session_loop::SessionLoop;
