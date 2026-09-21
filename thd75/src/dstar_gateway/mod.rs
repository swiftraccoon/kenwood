//! TH-D75 ownership and mode restoration around a shared D-STAR modem.
//!
//! [`DstarGateway`] either enters MMDVM transiently over CAT (`TN 3,x`, exited
//! with the matching `TN 0,x`) or takes over a connection that already carries
//! persistent binary-mode proof, which it never exits with an ASCII command.
//! Voice operations delegate to [`mmdvm::dstar`]; `DstarGateway::modem` borrows
//! the runtime read-only, so it cannot be swapped away from the CAT state
//! needed to restore the radio. Call `stop` to release the radio. Shared
//! configuration, events, headers and slow-data types come directly from
//! `mmdvm::dstar` and `dstar_gateway_core`.

pub mod gateway;

pub use crate::radio::mmdvm_session::{PersistentMmdvm, TransientMmdvm};
pub use gateway::DstarGateway;
