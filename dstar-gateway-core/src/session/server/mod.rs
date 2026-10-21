//! Server-side session machinery.
//!
//! All three protocols (`DExtra`, `DPlus`, `DCS`) are implemented at
//! the `ServerSessionCore` level. The `dstar-gateway-server` shell
//! dispatches all three through dedicated per-protocol
//! `handle_inbound_*` methods on `ProtocolEndpoint<P>`.
//!
//! [`ServerSessionCore`] checks the runtime state of one peer. The
//! `dstar-gateway-server` shell creates one [`ServerSessionCore`] per
//! inbound peer and routes datagrams through
//! [`ServerSessionCore::handle_input`].
//!
//! [`ForwardableFrame`] is a helper type describing an encoded voice
//! frame ready for fan-out to N other connected clients; the
//! fan-out engine uses it to avoid re-encoding the same bytes
//! per destination.

pub use self::core::ServerSessionCore;
pub use event::{ClientRejectedReason, ServerEvent};
pub use forwardable::ForwardableFrame;
pub use state::ServerStateKind;

mod core;
mod event;
mod forwardable;
mod state;
