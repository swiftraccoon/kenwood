//! Outbound packet priority queue keyed by `not_before: Instant`.
//!
//! The session's outbox holds outbound packets with optional
//! retransmission scheduling. `pop_ready(now)` returns the next
//! packet whose `not_before` is at or before `now`. Used by both
//! the immediate-send path and the retransmit scheduler.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::net::SocketAddr;
use std::time::Instant;

/// One queued outbound datagram with its earliest send instant.
#[derive(Debug, Clone)]
pub(crate) struct OutboundPacket {
    pub(crate) dst: SocketAddr,
    pub(crate) payload: Vec<u8>,
    pub(crate) not_before: Instant,
}

impl PartialEq for OutboundPacket {
    fn eq(&self, other: &Self) -> bool {
        self.not_before == other.not_before
    }
}
impl Eq for OutboundPacket {}
impl PartialOrd for OutboundPacket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OutboundPacket {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed ordering so BinaryHeap pops the EARLIEST first.
        other.not_before.cmp(&self.not_before)
    }
}

/// Priority queue of outbound packets ordered by `not_before`.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    queue: BinaryHeap<OutboundPacket>,
}

impl Outbox {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn enqueue(&mut self, packet: OutboundPacket) {
        self.queue.push(packet);
    }

    pub(crate) fn pop_ready(&mut self, now: Instant) -> Option<OutboundPacket> {
        if self.queue.peek().is_some_and(|p| p.not_before <= now) {
            self.queue.pop()
        } else {
            None
        }
    }

    pub(crate) fn peek_next_deadline(&self) -> Option<Instant> {
        self.queue.peek().map(|p| p.not_before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    #[test]
    fn empty_pop_returns_none() {
        let mut ob = Outbox::new();
        assert!(ob.pop_ready(Instant::now()).is_none());
    }

    #[test]
    fn pop_ready_returns_due_packet() -> Result<(), Box<dyn std::error::Error>> {
        let mut ob = Outbox::new();
        let now = Instant::now();
        ob.enqueue(OutboundPacket {
            dst: ADDR,
            payload: vec![1, 2, 3],
            not_before: now,
        });
        let popped = ob.pop_ready(now).ok_or("expected due packet")?;
        assert_eq!(popped.payload, vec![1, 2, 3]);
        assert_eq!(popped.dst, ADDR);
        assert!(
            ob.pop_ready(now).is_none(),
            "outbox drained after single pop"
        );
        Ok(())
    }

    #[test]
    fn pop_ready_holds_future_packet() {
        let mut ob = Outbox::new();
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        ob.enqueue(OutboundPacket {
            dst: ADDR,
            payload: vec![1],
            not_before: later,
        });
        assert!(ob.pop_ready(now).is_none());
        assert_eq!(ob.peek_next_deadline(), Some(later));
        assert!(ob.pop_ready(later).is_some());
    }

    #[test]
    fn earlier_packet_pops_first() -> Result<(), Box<dyn std::error::Error>> {
        let mut ob = Outbox::new();
        let now = Instant::now();
        ob.enqueue(OutboundPacket {
            dst: ADDR,
            payload: vec![2],
            not_before: now + Duration::from_millis(100),
        });
        ob.enqueue(OutboundPacket {
            dst: ADDR,
            payload: vec![1],
            not_before: now,
        });
        assert_eq!(
            ob.pop_ready(now + Duration::from_millis(200))
                .ok_or("expected first")?
                .payload,
            vec![1]
        );
        assert_eq!(
            ob.pop_ready(now + Duration::from_millis(200))
                .ok_or("expected second")?
                .payload,
            vec![2]
        );
        Ok(())
    }

    #[test]
    fn peek_next_deadline_reports_earliest() {
        let mut ob = Outbox::new();
        let now = Instant::now();
        ob.enqueue(OutboundPacket {
            dst: ADDR,
            payload: vec![],
            not_before: now + Duration::from_secs(5),
        });
        ob.enqueue(OutboundPacket {
            dst: ADDR,
            payload: vec![],
            not_before: now + Duration::from_secs(1),
        });
        assert_eq!(ob.peek_next_deadline(), Some(now + Duration::from_secs(1)));
    }
}
