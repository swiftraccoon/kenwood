//! Sealed `ClientState` markers for the typestate session.

/// Sealed marker for client connection states.
pub trait ClientState: sealed::Sealed + 'static {}

mod sealed {
    pub trait Sealed {}
}

/// Built but no I/O happened.
#[derive(Debug, Clone, Copy)]
pub struct Configured;

/// (`DPlus` only) TCP auth completed, host list cached.
#[derive(Debug, Clone, Copy)]
pub struct Authenticated;

/// LINK1 sent, awaiting LINK1-ACK or LINK2-ACK.
#[derive(Debug, Clone, Copy)]
pub struct Connecting;

/// Connected and operational. The only state where `send_*` exists.
#[derive(Debug, Clone, Copy)]
pub struct Connected;

/// UNLINK sent, awaiting confirmation or timeout.
#[derive(Debug, Clone, Copy)]
pub struct Disconnecting;

/// Terminal — must be rebuilt to use again.
#[derive(Debug, Clone, Copy)]
pub struct Closed;

impl sealed::Sealed for Configured {}
impl ClientState for Configured {}
impl sealed::Sealed for Authenticated {}
impl ClientState for Authenticated {}
impl sealed::Sealed for Connecting {}
impl ClientState for Connecting {}
impl sealed::Sealed for Connected {}
impl ClientState for Connected {}
impl sealed::Sealed for Disconnecting {}
impl ClientState for Disconnecting {}
impl sealed::Sealed for Closed {}
impl ClientState for Closed {}

/// Runtime discriminator mirroring the typestate markers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientStateKind {
    /// Configured.
    Configured,
    /// Authenticated (`DPlus` only).
    Authenticated,
    /// Connecting.
    Connecting,
    /// Connected.
    Connected,
    /// Disconnecting.
    Disconnecting,
    /// Closed.
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_distinct_zero_sized_types() {
        assert_eq!(size_of::<Configured>(), 0);
        assert_eq!(size_of::<Connected>(), 0);
        assert_eq!(size_of::<Closed>(), 0);
    }

    #[test]
    fn state_kind_variants_distinct() {
        assert_ne!(ClientStateKind::Configured, ClientStateKind::Connected);
    }
}
