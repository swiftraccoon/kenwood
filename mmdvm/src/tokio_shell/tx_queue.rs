// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Per-mode TX queue that only releases frames when the modem's
//! reported FIFO has enough slot space.
//!
//! Mirrors the `m_tx<MODE>Data` rings in `MMDVMHost/Modem.cpp`
//! (`Modem.cpp:288-315` for the per-mode ring buffers and
//! `Modem.cpp:1049-1220` for the space-gated drain logic).

use std::collections::VecDeque;

use mmdvm_core::{MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, ModemMode};

/// D-STAR header slot cost (`MMDVMHost` `Modem.cpp:1053`).
pub(crate) const DSTAR_HEADER_SLOTS: u8 = 4;
/// D-STAR data slot cost (`MMDVMHost` `Modem.cpp:1054`).
pub(crate) const DSTAR_DATA_SLOTS: u8 = 1;
/// D-STAR EOT slot cost (`MMDVMHost` `Modem.cpp:1055`).
pub(crate) const DSTAR_EOT_SLOTS: u8 = 1;

/// Maximum number of frames the host-side TX queue holds before
/// `push_*` refuses with [`TxQueueFull`].
///
/// Mirrors `MMDVMHost`'s fixed-size host-side ring
/// (`CRingBuffer m_txDStarData`, ~60 serialized data frames) — a
/// bounded queue gives callers backpressure instead of unbounded
/// memory growth when the modem stops granting FIFO space.
pub(crate) const MAX_TX_QUEUE_FRAMES: usize = 64;

/// The TX queue is at capacity; the frame was not enqueued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TxQueueFull;

impl std::fmt::Display for TxQueueFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TX queue is full ({MAX_TX_QUEUE_FRAMES} frames)")
    }
}

impl std::error::Error for TxQueueFull {}

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
    ///
    /// # Errors
    ///
    /// [`TxQueueFull`] if the queue holds [`MAX_TX_QUEUE_FRAMES`].
    pub(crate) fn push_dstar_header(&mut self, bytes: [u8; 41]) -> Result<(), TxQueueFull> {
        self.push(QueuedFrame {
            command: MMDVM_DSTAR_HEADER,
            payload: bytes.to_vec(),
            slots_required: DSTAR_HEADER_SLOTS,
            mode: ModemMode::DStar,
        })
    }

    /// Queue a D-STAR data frame (needs [`DSTAR_DATA_SLOTS`] slot).
    ///
    /// # Errors
    ///
    /// [`TxQueueFull`] if the queue holds [`MAX_TX_QUEUE_FRAMES`].
    pub(crate) fn push_dstar_data(&mut self, bytes: [u8; 12]) -> Result<(), TxQueueFull> {
        self.push(QueuedFrame {
            command: MMDVM_DSTAR_DATA,
            payload: bytes.to_vec(),
            slots_required: DSTAR_DATA_SLOTS,
            mode: ModemMode::DStar,
        })
    }

    /// Queue a D-STAR EOT (needs [`DSTAR_EOT_SLOTS`] slot).
    ///
    /// # Errors
    ///
    /// [`TxQueueFull`] if the queue holds [`MAX_TX_QUEUE_FRAMES`].
    pub(crate) fn push_dstar_eot(&mut self) -> Result<(), TxQueueFull> {
        self.push(QueuedFrame {
            command: MMDVM_DSTAR_EOT,
            payload: Vec::new(),
            slots_required: DSTAR_EOT_SLOTS,
            mode: ModemMode::DStar,
        })
    }

    /// Enqueue with the capacity check.
    fn push(&mut self, frame: QueuedFrame) -> Result<(), TxQueueFull> {
        if self.dstar.len() >= MAX_TX_QUEUE_FRAMES {
            return Err(TxQueueFull);
        }
        self.dstar.push_back(frame);
        Ok(())
    }

    /// Pop the next D-STAR frame if strictly more slots are free than
    /// the frame costs. Returns `None` if the queue is empty or the
    /// FIFO lacks headroom for the head frame.
    ///
    /// The strict `>` mirrors the reference
    /// (`MMDVMHost/Modem.cpp:1053-1055`: header needs `> 4`, data and
    /// EOT need `> 1`) — one slot of margin absorbs estimation drift
    /// between 250 ms status polls and prevents modem TX-FIFO
    /// overflow.
    pub(crate) fn pop_if_space_allows(&mut self, dstar_space: u8) -> Option<QueuedFrame> {
        let head = self.dstar.front()?;
        if dstar_space > head.slots_required {
            self.dstar.pop_front()
        } else {
            None
        }
    }

    /// Is the queue empty?
    pub(crate) fn is_empty(&self) -> bool {
        self.dstar.is_empty()
    }

    /// Total queued frames across all modes.
    pub(crate) fn len(&self) -> usize {
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
        q.push_dstar_header([0u8; 41])?;
        // Reference gating is strict `>`: a header (cost 4) drains
        // only when more than 4 slots are free (Modem.cpp:1053).
        let head = q
            .pop_if_space_allows(DSTAR_HEADER_SLOTS + 1)
            .ok_or("expected frame")?;
        assert_eq!(head.command, MMDVM_DSTAR_HEADER);
        assert_eq!(head.slots_required, DSTAR_HEADER_SLOTS);
        assert_eq!(head.mode, ModemMode::DStar);
        assert!(q.is_empty());
        Ok(())
    }

    #[test]
    fn pop_at_exact_slot_cost_returns_none() -> TestResult {
        // The reference always keeps one slot of margin: at exactly
        // the frame's cost the frame must stay queued.
        let mut q = TxQueue::new();
        q.push_dstar_header([0u8; 41])?;
        assert!(q.pop_if_space_allows(DSTAR_HEADER_SLOTS).is_none());
        assert_eq!(q.len(), 1, "frame must stay enqueued");
        Ok(())
    }

    #[test]
    fn header_needs_more_than_4_data_more_than_1() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_header([0u8; 41])?;
        q.push_dstar_data([0u8; 12])?;

        // Header can't drain at 4 (strict `>`), drains at 5.
        assert!(q.pop_if_space_allows(4).is_none());
        let h = q.pop_if_space_allows(5).ok_or("expected header")?;
        assert_eq!(h.slots_required, 4);

        // Data can't drain at 1, drains at 2.
        assert!(q.pop_if_space_allows(1).is_none());
        let d = q.pop_if_space_allows(2).ok_or("expected data")?;
        assert_eq!(d.command, MMDVM_DSTAR_DATA);
        Ok(())
    }

    #[test]
    fn eot_needs_more_than_1_slot() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_eot()?;
        assert!(q.pop_if_space_allows(1).is_none());
        let eot = q.pop_if_space_allows(2).ok_or("expected eot")?;
        assert_eq!(eot.command, MMDVM_DSTAR_EOT);
        assert!(eot.payload.is_empty());
        Ok(())
    }

    #[test]
    fn queue_caps_at_max_frames() -> TestResult {
        let mut q = TxQueue::new();
        for _ in 0..MAX_TX_QUEUE_FRAMES {
            q.push_dstar_data([0u8; 12])
                .map_err(|e| format!("premature TxQueueFull: {e}"))?;
        }
        assert!(
            q.push_dstar_data([0u8; 12]).is_err(),
            "push beyond MAX_TX_QUEUE_FRAMES must be rejected"
        );
        Ok(())
    }

    #[test]
    fn fifo_order_preserved() -> TestResult {
        let mut q = TxQueue::new();
        q.push_dstar_header([1u8; 41])?;
        q.push_dstar_data([2u8; 12])?;
        q.push_dstar_data([3u8; 12])?;
        q.push_dstar_eot()?;

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
