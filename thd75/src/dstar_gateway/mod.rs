//! TH-D75 ownership and mode restoration around a shared D-STAR modem.
//!
//! [`DstarGateway`] enters the qualified transient mode or consumes existing
//! persistent binary-link proof. Its voice operations delegate to
//! [`mmdvm::dstar`] without exposing the owned runtime for replacement.
//! [`DstarGateway::modem`] provides read-only inspection. Stop the model owner
//! to apply the proper exit policy and recover radio state. Shared
//! configuration, events, headers and slow-data types come directly from
//! `mmdvm::dstar` and `dstar_gateway_core`.

pub mod gateway;

pub use crate::radio::mmdvm_session::{PersistentMmdvm, TransientMmdvm};
pub use gateway::DstarGateway;
