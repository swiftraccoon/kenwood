//! Client-side session machinery.

pub use any_session::AnySession;
pub use builder::{Missing, Provided, SessionBuilder};
pub use config::{ClientProtocol, SessionConfig};
pub use core::SessionCore;
pub use event::{DisconnectReason, Event, VoiceEndReason};
pub use failed::Failed;
pub use protocol::{DExtra, DPlus, Dcs, NoAuthRequired, Protocol};
pub use session::Session;
pub use state::{
    Authenticated, ClientState, ClientStateKind, Closed, Configured, Connected, Connecting,
    Disconnecting,
};

mod any_session;
mod builder;
mod config;
mod core;
mod event;
mod failed;
mod protocol;
mod session;
mod state;
