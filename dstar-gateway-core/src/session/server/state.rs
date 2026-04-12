//! Sealed `ServerState` markers for the server-side typestate.
//!
//! Each marker is a zero-sized type that gates which methods are
//! available on a [`super::ServerSession`]. The seal prevents
//! downstream crates from adding their own marker types, which would
//! break the typestate's exhaustiveness proof.
//!
//! [`ServerStateKind`] is the runtime discriminator that mirrors the
//! compile-time markers — used in error variants, diagnostics, and
//! any path where the `S` phantom has been erased.

/// Sealed marker for server-side session states.
pub trait ServerState: sealed::Sealed + 'static {}

mod sealed {
    pub trait Sealed {}
}

/// New client packet seen, no link request validated yet.
#[derive(Debug, Clone, Copy)]
pub struct Unknown;

/// `DPlus`-specific: LINK1 seen and acknowledged, waiting for LINK2.
///
/// This state only applies to `DPlus` sessions — `DExtra` and `DCS`
/// link in a single packet and move directly from [`Unknown`] to
/// [`Linked`]. The public runtime discriminator
/// [`ServerStateKind`] collapses `Link1Received` into
/// [`ServerStateKind::Unknown`] because external consumers typically
/// only care about "not linked yet" vs "linked", not the details
/// of the `DPlus` two-step handshake.
#[derive(Debug, Clone, Copy)]
pub struct Link1Received;

/// LINK request received and authorized, client is linked.
#[derive(Debug, Clone, Copy)]
pub struct Linked;

/// Voice stream in progress on this client.
#[derive(Debug, Clone, Copy)]
pub struct Streaming;

/// UNLINK request seen, client being torn down.
#[derive(Debug, Clone, Copy)]
pub struct Unlinking;

/// Terminal — client disconnected.
#[derive(Debug, Clone, Copy)]
pub struct Closed;

impl sealed::Sealed for Unknown {}
impl ServerState for Unknown {}
impl sealed::Sealed for Link1Received {}
impl ServerState for Link1Received {}
impl sealed::Sealed for Linked {}
impl ServerState for Linked {}
impl sealed::Sealed for Streaming {}
impl ServerState for Streaming {}
impl sealed::Sealed for Unlinking {}
impl ServerState for Unlinking {}
impl sealed::Sealed for Closed {}
impl ServerState for Closed {}

/// Runtime discriminator mirroring the typestate markers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerStateKind {
    /// Unknown.
    Unknown,
    /// Linked.
    Linked,
    /// Streaming.
    Streaming,
    /// Unlinking.
    Unlinking,
    /// Closed.
    Closed,
}

#[cfg(test)]
mod tests {
    use super::{Closed, Linked, ServerStateKind, Streaming, Unknown, Unlinking};

    #[test]
    fn states_are_zero_sized() {
        assert_eq!(size_of::<Unknown>(), 0);
        assert_eq!(size_of::<Linked>(), 0);
        assert_eq!(size_of::<Streaming>(), 0);
        assert_eq!(size_of::<Unlinking>(), 0);
        assert_eq!(size_of::<Closed>(), 0);
    }

    #[test]
    fn state_kind_variants_distinct() {
        assert_ne!(ServerStateKind::Unknown, ServerStateKind::Linked);
        assert_ne!(ServerStateKind::Linked, ServerStateKind::Streaming);
        assert_ne!(ServerStateKind::Streaming, ServerStateKind::Unlinking);
        assert_ne!(ServerStateKind::Unlinking, ServerStateKind::Closed);
    }
}
