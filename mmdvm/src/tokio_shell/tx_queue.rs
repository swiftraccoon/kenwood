// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Per-mode TX queue that only releases frames when the modem's
//! reported FIFO has enough slot space.
//!
//! Mirrors the `m_tx<MODE>Data` rings in `ref/MMDVMHost/Modem.cpp`
//! (`Modem.cpp:288-315` for the per-mode ring buffers and
//! `Modem.cpp:1049-1220` for the space-gated drain logic).

// TxQueue is crate-internal: the handle never sees it. The `pub(crate)`
// surface is required so the sibling `modem_loop.rs` can construct
// and drive it, but it's not intended to leak into the public API.
#![expect(
    clippy::redundant_pub_crate,
    reason = "TxQueue and friends are crate-internal infrastructure"
)]

use std::collections::VecDeque;

use mmdvm_core::{MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, ModemMode};

/// D-STAR header slot cost (`MMDVMHost` `Modem.cpp:1053`).
pub(crate) const DSTAR_HEADER_SLOTS: u8 = 4;
/// D-STAR data slot cost (`MMDVMHost` `Modem.cpp:1054`).
pub(crate) const DSTAR_DATA_SLOTS: u8 = 1;
/// D-STAR EOT slot cost (`MMDVMHost` `Modem.cpp:1055`).
pub(crate) const DSTAR_EOT_SLOTS: u8 = 1;

/// Single queued TX frame with its mode attribution.
#[derive(Debug, Clone)]
pub(crate) struct QueuedFrame {
    /// MMDVM command byte.
    pub(crate) command: u8,
    /// Raw payload bytes.
    pub(crate) payload: Vec<u8>,
    /// How many FIFO slots this frame costs on the modem.
    pub(crate) slots_required: u8,
    /// Which modem mode the frame belongs to.
    pub(crate) mode: ModemMode,
}

/// Per-mode FIFO of pending TX frames, drained by the modem loop
/// when the modem's reported buffer space allows.
///
/// Mirrors `MMDVMHost`'s pattern where each mode has its own
/// `CRingBuffer<unsigned char> m_tx<MODE>Data` drained by
/// `Modem.cpp:1049-1220` based on the corresponding `m_<mode>Space`
/// counter.
#[derive(Debug, Default)]
pub(crate) struct TxQueue {
    dstar: VecDeque<QueuedFrame>,
    // Future modes: dmr_slot1, dmr_slot2, ysf, p25, nxdn, pocsag, fm.
}

impl TxQueue {
    /// Construct an empty queue.
    pub(crate) const fn new() -> Self {
        Self {
            dstar: VecDeque::new(),
        }
    }

    /// Queue a D-STAR header (needs [`DSTAR_HEADER_SLOTS`] slots).
    pub(crate) fn push_dstar_header(&mut self, bytes: [u8; 41]) {
        self.dstar.push_back(QueuedFrame {
            command: MMDVM_DSTAR_HEADER,
            payload: bytes.to_vec(),
            slots_required: DSTAR_HEADER_SLOTS,
            mode: ModemMode::DStar,
        });
    }

    /// Queue a D-STAR data frame (needs [`DSTAR_DATA_SLOTS`] slot).
    pub(crate) fn push_dstar_data(&mut self, bytes: [u8; 12]) {
        self.dstar.push_back(QueuedFrame {
            command: MMDVM_DSTAR_DATA,
            payload: bytes.to_vec(),
            slots_required: DSTAR_DATA_SLOTS,
            mode: ModemMode::DStar,
        });
    }

    /// Queue a D-STAR EOT (needs [`DSTAR_EOT_SLOTS`] slot).
    pub(crate) fn push_dstar_eot(&mut self) {
        self.dstar.push_back(QueuedFrame {
            command: MMDVM_DSTAR_EOT,
            payload: Vec::new(),
            slots_required: DSTAR_EOT_SLOTS,
            mode: ModemMode::DStar,
        });
    }

    /// Pop the next D-STAR frame if `dstar_space` slots available is
    /// at least the frame's cost. Returns `None` if the queue is
    /// empty or the FIFO is too full for the head frame.
    pub(crate) fn pop_if_space_allows(&mut self, dstar_space: u8) -> Option<QueuedFrame> {
        let head = self.dstar.front()?;
        if dstar_space >= head.slots_required {
            self.dstar.pop_front()
        } else {
            None
        }
    }

    /// Is the queue empty?
    pub(crate) fn is_empty(&self) -> bool {
        self.dstar.is_empty()
    }

    /// Current D-STAR queue depth (for telemetry / tests).
    #[cfg(test)]
    pub(crate) fn dstar_len(&self) -> usize {
        self.dstar.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn push_then_pop_with_space_returns_frame() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_header([0u8; 41]);
        let head = q
            .pop_if_space_allows(DSTAR_HEADER_SLOTS)
            .ok_or("expected frame")?;
        assert_eq!(head.command, MMDVM_DSTAR_HEADER);
        assert_eq!(head.slots_required, DSTAR_HEADER_SLOTS);
        assert_eq!(head.mode, ModemMode::DStar);
        assert!(q.is_empty());
        Ok(())
    }

    #[test]
    fn push_then_pop_with_insufficient_space_returns_none() {
        let mut q = TxQueue::new();
        q.push_dstar_header([0u8; 41]);
        // Header costs 4 slots — only 3 reported.
        assert!(q.pop_if_space_allows(3).is_none());
        assert_eq!(q.dstar_len(), 1, "frame must stay enqueued");
    }

    #[test]
    fn header_requires_4_slots_data_requires_1() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_header([0u8; 41]);
        q.push_dstar_data([0u8; 12]);

        // With 1 slot only, header can't drain.
        assert!(q.pop_if_space_allows(1).is_none());
        // With 4 slots, header drains, leaving data.
        let h = q.pop_if_space_allows(4).ok_or("expected header")?;
        assert_eq!(h.slots_required, 4);

        // Now the data frame at head requires only 1 slot.
        let d = q.pop_if_space_allows(1).ok_or("expected data")?;
        assert_eq!(d.command, MMDVM_DSTAR_DATA);
        Ok(())
    }

    #[test]
    fn eot_requires_1_slot() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_eot();
        assert!(q.pop_if_space_allows(0).is_none());
        let eot = q.pop_if_space_allows(1).ok_or("expected eot")?;
        assert_eq!(eot.command, MMDVM_DSTAR_EOT);
        assert!(eot.payload.is_empty());
        Ok(())
    }

    #[test]
    fn fifo_order_preserved() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_header([1u8; 41]);
        q.push_dstar_data([2u8; 12]);
        q.push_dstar_data([3u8; 12]);
        q.push_dstar_eot();

        // Large space so every pop succeeds.
        let f1 = q.pop_if_space_allows(255).ok_or("f1")?;
        assert_eq!(f1.command, MMDVM_DSTAR_HEADER);
        assert_eq!(f1.payload.first().copied(), Some(1));
        let f2 = q.pop_if_space_allows(255).ok_or("f2")?;
        assert_eq!(f2.command, MMDVM_DSTAR_DATA);
        assert_eq!(f2.payload.first().copied(), Some(2));
        let f3 = q.pop_if_space_allows(255).ok_or("f3")?;
        assert_eq!(f3.command, MMDVM_DSTAR_DATA);
        assert_eq!(f3.payload.first().copied(), Some(3));
        let f4 = q.pop_if_space_allows(255).ok_or("f4")?;
        assert_eq!(f4.command, MMDVM_DSTAR_EOT);
        assert!(q.is_empty());
        Ok(())
    }
}
