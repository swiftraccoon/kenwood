// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Tokio async shell for the MMDVM digital voice modem protocol.
//!
//! Builds on the sans-io [`mmdvm-core`] crate to provide an async
//! handle-and-loop architecture for talking to MMDVM modems like the
//! Kenwood TH-D75, Pi-Star hotspots, `ZumSpot`, and similar hardware.
//!
//! The top-level entry point is [`tokio_shell::AsyncModem::spawn`].
//!
//! Mirrors the reference C++ implementation at `ref/MMDVMHost/`:
//! periodic 250 ms `GetStatus` polls correct local buffer-space
//! estimates, and per-mode TX queues are drained only when the
//! modem reports FIFO slot availability.
//!
//! [`mmdvm-core`]: https://github.com/swiftraccoon/kenwood/tree/main/mmdvm-core

pub mod error;
pub mod tokio_shell;
pub mod transport;

pub use error::ShellError;
pub use mmdvm_core as core;
pub use tokio_shell::{AsyncModem, Event};
pub use transport::Transport;
