//! Sans-io `Driver` trait.

use std::net::SocketAddr;
use std::time::Instant;

/// One outbound datagram from the sans-io core.
#[derive(Debug, Clone)]
pub struct Transmit<'a> {
    /// Destination address.
    pub dst: SocketAddr,
    /// Wire bytes (borrowed from the session's internal scratch buffer).
    pub payload: &'a [u8],
}

impl<'a> Transmit<'a> {
    /// Construct a transmit from a destination + borrowed payload.
    #[must_use]
    pub const fn new(dst: SocketAddr, payload: &'a [u8]) -> Self {
        Self { dst, payload }
    }
}

/// Sans-io driver. Mirrors `quinn-proto::Connection` and
/// `rustls::Connection`.
///
/// The shell calls these methods in a loop. The core never calls
/// back into the shell. **Time is injected via the `now: Instant`
/// parameter on every call** — implementors of this trait must NOT
/// consult `Instant::now()` themselves.
pub trait Driver {
    /// Consumer-visible event type.
    type Event;
    /// Per-protocol structured error type.
    type Error;

    /// Feed an inbound datagram. Parses, advances state, may push
    /// events / outbound packets / timer updates.
    ///
    /// # Errors
    ///
    /// Returns the protocol-specific error if the bytes cannot be
    /// parsed.
    fn handle_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;

    /// Tell the core that wall time has advanced.
    fn handle_timeout(&mut self, now: Instant);

    /// Pop the next outbound datagram, if any.
    fn poll_transmit(&mut self, now: Instant) -> Option<Transmit<'_>>;

    /// Pop the next consumer-visible event.
    fn poll_event(&mut self) -> Option<Self::Event>;

    /// Earliest instant at which the core needs to be re-entered.
    ///
    /// `None` means "no pending timer — only wake me on input".
    fn poll_timeout(&self) -> Option<Instant>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    #[test]
    fn transmit_new_constructs() {
        let payload: &[u8] = &[0x05, 0x00, 0x18, 0x00, 0x01];
        let tx = Transmit::new(ADDR, payload);
        assert_eq!(tx.dst, ADDR);
        assert_eq!(tx.payload, payload);
    }
}
