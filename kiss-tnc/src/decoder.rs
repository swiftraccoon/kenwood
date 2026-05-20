//! Streaming KISS frame decoder for serial-byte reassembly.

use alloc::vec::Vec;

use crate::command::FEND;
use crate::error::KissError;
use crate::frame::{KissFrame, decode_kiss_frame};

/// Default maximum length, in bytes, of a complete KISS frame including
/// both FEND delimiters.
///
/// Used by [`KissDecoder::new`]. This comfortably exceeds a
/// maximum-size AX.25 frame even after worst-case KISS byte stuffing;
/// raise it with [`KissDecoder::with_max_frame_len`] if a transport
/// needs larger frames.
pub const DEFAULT_MAX_FRAME_LEN: usize = 1024;

/// Streaming KISS frame decoder.
///
/// Accepts arbitrary byte chunks from a serial transport via
/// [`Self::push`] and yields complete frames one at a time via
/// [`Self::next_frame`].
///
/// To bound memory against a stuck or noisy peer, the decoder discards
/// any frame — or run of bytes with no usable delimiter — longer than
/// its configured maximum (see [`Self::with_max_frame_len`]) and reports
/// [`KissError::FrameTooLong`].
#[derive(Debug)]
pub struct KissDecoder {
    /// Accumulated bytes since the last complete frame.
    buffer: Vec<u8>,
    /// `true` once we've seen a leading FEND and are inside a frame.
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
    #[must_use]
    pub const fn new() -> Self {
        Self::with_max_frame_len(DEFAULT_MAX_FRAME_LEN)
    }

    /// Create a new empty decoder with an explicit maximum frame length.
    ///
    /// `max_frame_len` bounds a complete frame *including* its two FEND
    /// delimiters. Frames — or runs of bytes with no usable delimiter —
    /// longer than this are discarded and reported as
    /// [`KissError::FrameTooLong`], which caps the decoder's memory use
    /// against a peer that never closes a frame. Values below `3` reject
    /// every frame, since the shortest possible frame is `FEND <type> FEND`.
    #[must_use]
    pub const fn with_max_frame_len(max_frame_len: usize) -> Self {
        Self {
            buffer: Vec::new(),
            in_frame: false,
            max_frame_len,
        }
    }

    /// Feed bytes from the transport into the decoder.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Try to extract the next complete frame from the buffer.
    ///
    /// Returns `Ok(None)` if the buffer does not yet contain a full
    /// frame, `Ok(Some(frame))` when one is available, and `Err(...)`
    /// on malformed input.
    ///
    /// # Errors
    ///
    /// Returns [`KissError::FrameTooLong`] when a frame — or a run of
    /// bytes with no usable delimiter — exceeds the configured maximum
    /// length; the offending bytes are discarded and decoding resyncs at
    /// the next FEND. Returns other [`KissError`] variants on invalid
    /// escape sequences or otherwise malformed frames.
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
                // can never close validly — discard it and resync.
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
            let frame_bytes: Vec<u8> = self.buffer.get(..=end_idx).unwrap_or(&[][..]).to_vec();
            // Drain up to but NOT including the closing FEND. Per the KISS
            // spec a single FEND both closes one frame and opens the next,
            // so the closing FEND is left in the buffer to serve as the
            // following frame's opening delimiter. `in_frame` therefore
            // stays `true`: once synced we never re-treat bytes as garbage.
            drop(self.buffer.drain(..end_idx));
            return decode_kiss_frame(&frame_bytes).map(Some);
        }
    }
}
