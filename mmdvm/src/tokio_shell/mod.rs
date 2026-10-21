//! Tokio async shell driving the sans-io [`mmdvm_core`] codec.
//!
//! Entry points:
//! - [`AsyncModem`]: user-facing handle over a spawned modem task;
//!   use [`AsyncModem::spawn`] to wire up the internal loop
//! - [`Event`]: inbound events the modem loop emits to consumers
//!
//! Internal types (`Request`, `ModemLoop`, `TxQueue`) are
//! crate-private.
//!
//! [`mmdvm_core`]: crate::core

pub(crate) use command::Request;
pub use configuration::{
    ConfigurationError, ConfigurationProfile, DstarConfiguration, SignalLevel, SignalLevels,
    TxDelay,
};
pub use event::Event;
pub use handle::AsyncModem;
pub(crate) use modem_loop::ModemLoop;
pub(crate) use shutdown::TaskCompletion;
pub use shutdown::{ModemShutdown, ShutdownFailure, ShutdownSummary};
pub(crate) use tx_queue::TxQueue;

mod command;
mod configuration;
mod event;
mod handle;
mod modem_loop;
mod shutdown;
mod tx_queue;
