//! Streaming KISS frame decoder for serial-byte reassembly.

use alloc::vec::Vec;

use crate::command::FEND;
use crate::error::KissError;
use crate::frame::KissFrame;

/// Default maximum length, in bytes, of a complete KISS frame including
/// both FEND delimiters.
///
/// Used by [`KissDecoder::new`]. Counts stuffed wire bytes, including the
/// type byte and both delimiters.
/// See the [limit example](crate#limits-and-errors).
pub const DEFAULT_MAX_FRAME_LEN: usize = 1024;

/// Streaming KISS frame decoder.
///
/// Accepts arbitrary byte chunks from a serial transport via
/// [`Self::push`] and yields complete frames one at a time via
/// [`Self::next_frame`].
///
/// [`Self::next_frame`] rejects complete frames exceeding the configured
/// wire length. It also discards oversized buffered input when a frame is
/// incomplete or no FEND is present. [`Self::push`] copies its entire input;
/// queued frames can together exceed the frame limit, and discarded bytes
/// leave the buffer's allocated capacity available for reuse. Callers control
/// queued input by choosing chunk sizes and draining frames between pushes.
/// See the [streaming example](crate#streaming) and
/// [limit example](crate#limits-and-errors).
#[derive(Debug)]
pub struct KissDecoder {
    /// Accumulated bytes since the last complete frame.
    buffer: Vec<u8>,
    /// `true` once a leading FEND has been seen and a frame is open.
    in_frame: bool,
    /// Maximum complete-frame length (both FENDs included) the decoder
    /// accepts; longer frames are discarded as [`KissError::FrameTooLong`].
    max_frame_len: usize,
}

impl Default for KissDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl KissDecoder {
    /// Create a new empty decoder with the [`DEFAULT_MAX_FRAME_LEN`]
    /// frame-length cap.
    /// See the [streaming example](crate#streaming).
    #[must_use]
    pub const fn new() -> Self {
        Self::with_max_frame_len(DEFAULT_MAX_FRAME_LEN)
    }

    /// Create a new empty decoder with an explicit maximum frame length.
    ///
    /// `max_frame_len` limits each complete frame's stuffed wire length,
    /// including its two FEND delimiters. [`Self::next_frame`] checks this
    /// limit; [`Self::push`] retains every supplied byte until decoding.
    /// Values below `3` reject every frame, since the shortest possible
    /// frame is `FEND <type> FEND`.
    /// See the [limit example](crate#limits-and-errors).
    #[must_use]
    pub const fn with_max_frame_len(max_frame_len: usize) -> Self {
        Self {
            buffer: Vec::new(),
            in_frame: false,
            max_frame_len,
        }
    }

    /// Copies an entire input chunk after any bytes already buffered.
    ///
    /// Call [`Self::next_frame`] to decode or reject buffered frames. Multiple
    /// complete frames may be queued even when their total length exceeds
    /// the per-frame limit. Allocated buffer capacity is retained after
    /// decoding or discarding input. See the [streaming example](crate#streaming).
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Try to extract the next complete frame from the buffer.
    ///
    /// Returns `Ok(None)` if the buffer does not yet contain a full
    /// frame, `Ok(Some(frame))` when one is available, and `Err(...)`
    /// on malformed input.
    ///
    /// Leading bytes before the first FEND and empty delimiter pairs are
    /// skipped. A completed frame consumes its body and preserves its closing
    /// FEND as the next frame's opening delimiter, including after an error.
    /// Incomplete oversized input is cleared; subsequent input is scanned for
    /// a new opening FEND.
    /// See the [error recovery example](crate#limits-and-errors).
    ///
    /// # Errors
    ///
    /// Returns [`KissError::FrameTooLong`] when a complete frame exceeds the
    /// configured wire-length limit, or when buffered input exceeding that
    /// limit has no FEND or lacks a closing FEND. Returns
    /// [`KissError::UnknownCommand`], [`KissError::InvalidEscapeSequence`], or
    /// [`KissError::TruncatedEscapeSequence`] for a malformed completed frame.
    /// The rejected bytes are consumed, so callers can continue decoding.
    pub fn next_frame(&mut self) -> Result<Option<KissFrame>, KissError> {
        loop {
            // Find the opening FEND.
            let Some(first) = self.buffer.iter().position(|&b| b == FEND) else {
                // No delimiter anywhere. Discard if the buffer has
                // already outgrown any frame we could ever accept.
                if self.buffer.len() > self.max_frame_len {
                    self.buffer.clear();
                    self.in_frame = false;
                    return Err(KissError::FrameTooLong);
                }
                return Ok(None);
            };
            if !self.in_frame {
                // Discard any pre-FEND garbage and start a new frame.
                drop(self.buffer.drain(..first));
                self.in_frame = true;
            }
            // A leading FEND now sits at buffer[0]; find the closing FEND.
            let tail = self.buffer.get(1..).unwrap_or(&[][..]);
            let Some(end) = tail.iter().position(|&b| b == FEND) else {
                // Frame still open. If it has already outgrown the cap it
                // can never close validly: discard it and resync.
                if self.buffer.len() > self.max_frame_len {
                    self.buffer.clear();
                    self.in_frame = false;
                    return Err(KissError::FrameTooLong);
                }
                return Ok(None);
            };
            let end_idx = end + 1;
            // Empty frame (`FEND FEND`)? Skip and stay in-frame.
            if end_idx == 1 {
                drop(self.buffer.drain(..1));
                continue;
            }
            // A complete frame spans buffer[0..=end_idx] (both FENDs).
            let frame_len = end_idx + 1;
            if frame_len > self.max_frame_len {
                // Over-long but complete: discard it, keeping the closing
                // FEND so the next frame can still be found, then resync.
                drop(self.buffer.drain(..end_idx));
                return Err(KissError::FrameTooLong);
            }
            // Slice the complete frame including both FENDs.
            let frame = KissFrame::from_bytes(self.buffer.get(..=end_idx).unwrap_or(&[][..]));
            // Drain up to but NOT including the closing FEND. Per the KISS
            // spec a single FEND both closes one frame and opens the next,
            // so the closing FEND is left in the buffer to serve as the
            // following frame's opening delimiter. `in_frame` therefore
            // stays `true`: once synced we never re-treat bytes as garbage.
            drop(self.buffer.drain(..end_idx));
            return frame.map(Some);
        }
    }
}
