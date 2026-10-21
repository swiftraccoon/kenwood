//! Observable states of one reflector client.

/// Runtime state of [`super::ServerSessionCore`].
///
/// `DPlus` exposes both pre-link handshake phases as [`Self::Unknown`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerStateKind {
    /// No completed link handshake.
    Unknown,
    /// Linked and waiting for a voice header.
    Linked,
    /// A voice stream is active.
    Streaming,
    /// An unlink request is being completed.
    Unlinking,
    /// The client is disconnected; subsequent input is ignored.
    Closed,
}
