//! Outbound packet priority queue keyed by `not_before: Instant`.
//!
//! The session's outbox holds outbound packets with optional
//! retransmission scheduling. `pop_ready(now)` returns the next
//! packet whose `not_before` is at or before `now`. Used by both
//! the immediate-send path and the retransmit scheduler.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::time::Instant;

/// One queued outbound datagram with its earliest send instant.
#[derive(Debug, Clone)]
pub(crate) struct OutboundPacket {
    pub(crate) dst: SocketAddr,
    pub(crate) payload: Vec<u8>,
    pub(crate) not_before: Instant,
}

/// Packets ordered by deadline, then by insertion within each deadline.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    queue: BTreeMap<Instant, VecDeque<OutboundPacket>>,
}

impl Outbox {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn enqueue(&mut self, packet: OutboundPacket) {
        self.queue
            .entry(packet.not_before)
            .or_default()
            .push_back(packet);
    }

    pub(crate) fn pop_ready(&mut self, now: Instant) -> Option<OutboundPacket> {
        let mut entry = self.queue.first_entry()?;
        if *entry.key() > now {
            return None;
        }
        let packet = entry.get_mut().pop_front();
        if entry.get().is_empty() {
            let _removed = entry.remove();
        }
        packet
    }

    pub(crate) fn peek_next_deadline(&self) -> Option<Instant> {
        self.queue.first_key_value().map(|(&deadline, _)| deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    #[test]
    fn same_deadline_preserves_header_data_and_eot_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut outbox = Outbox::new();
        let now = Instant::now();
        for index in 0_u8..64 {
            outbox.enqueue(OutboundPacket {
                dst: ADDR,
                payload: vec![index],
                not_before: now,
            });
        }
        for expected in 0_u8..64 {
            let packet = outbox.pop_ready(now).ok_or("queued packet missing")?;
            assert_eq!(
                packet.payload,
                [expected],
                "equal deadlines preserve insertion order"
            );
        }
        assert!(
            outbox.pop_ready(now).is_none(),
            "all queued packets were returned"
        );
        Ok(())
    }

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
