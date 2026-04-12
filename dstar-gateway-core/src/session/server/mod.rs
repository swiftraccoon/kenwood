//! Server-side session machinery.
//!
//! All three protocols (`DExtra`, `DPlus`, `DCS`) are implemented at
//! the `ServerSessionCore` level. The `dstar-gateway-server` shell
//! dispatches all three through dedicated per-protocol
//! `handle_inbound_*` methods on `ProtocolEndpoint<P>`.
//!
//! The server-side machinery mirrors the client-side split:
//! [`ServerSessionCore`] is the protocol-erased state machine,
//! [`ServerSession<P, S>`] is the typestate wrapper. The
//! `dstar-gateway-server` shell spawns one [`ServerSessionCore`] per
//! inbound peer and routes datagrams through
//! [`ServerSessionCore::handle_input`].
//!
//! [`ForwardableFrame`] is a helper type describing an encoded voice
//! frame ready for fan-out to N other connected clients — the
//! fan-out engine uses it to avoid re-encoding the same bytes
//! per destination.

mod core;
mod event;
mod forwardable;
mod session;
mod state;

pub use self::core::ServerSessionCore;
pub use event::{ClientRejectedReason, ServerEvent};
pub use forwardable::ForwardableFrame;
pub use session::ServerSession;
pub use state::{
    Closed, Link1Received, Linked, ServerState, ServerStateKind, Streaming, Unknown, Unlinking,
};
