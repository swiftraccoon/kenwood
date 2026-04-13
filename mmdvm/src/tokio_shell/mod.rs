//! Tokio async shell driving the sans-io [`mmdvm_core`] codec.
//!
//! Entry points:
//! - [`AsyncModem`] — user-facing handle over a spawned modem task;
//!   use [`AsyncModem::spawn`] to wire up the internal loop
//! - [`Event`] — inbound events the modem loop emits to consumers
//!
//! Internal types (`Command`, `ModemLoop`, `TxQueue`) are
//! crate-private.
//!
//! [`mmdvm_core`]: crate::core

mod command;
mod event;
mod handle;
mod modem_loop;
mod tx_queue;

pub(crate) use command::Command;
pub use event::Event;
pub use handle::AsyncModem;
pub(crate) use modem_loop::ModemLoop;
pub(crate) use tx_queue::TxQueue;
