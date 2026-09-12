// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

#![doc = include_str!("../README.md")]

#[cfg(feature = "dstar")]
pub mod dstar;
pub mod error;
#[cfg(feature = "probe")]
pub mod probe;
#[cfg(feature = "runtime")]
pub mod tokio_shell;
pub mod transport;

pub use error::ShellError;
pub use mmdvm_core as core;
#[cfg(feature = "runtime")]
pub use tokio_shell::{AsyncModem, Event};
pub use transport::Transport;

#[cfg(all(test, not(any(feature = "dstar", feature = "probe"))))]
use kenwood_transport as _;

#[cfg(not(any(feature = "runtime", feature = "probe")))]
use tracing as _;
