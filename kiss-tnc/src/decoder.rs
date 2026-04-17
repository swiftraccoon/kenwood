//! Streaming KISS frame decoder for serial-byte reassembly.

use alloc::vec::Vec;

use crate::command::FEND;
use crate::error::KissError;
use crate::frame::{KissFrame, decode_kiss_frame};

/// Streaming KISS frame decoder.
///
/// Accepts arbitrary byte chunks from a serial transport via
/// [`Self::push`] and yields complete frames one at a time via
/// [`Self::next_frame`].
#[derive(Debug, Default)]
pub struct KissDecoder {
    /// Accumulated bytes since the last complete frame.
    buffer: Vec<u8>,
    /// `true` once we've seen a leading FEND and are inside a frame.
    in_frame: bool,
}

impl KissDecoder {
    /// Create a new empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            in_frame: false,
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
    /// Returns [`KissError`] on invalid escape sequences or other
    /// frame-level errors.
    pub fn next_frame(&mut self) -> Result<Option<KissFrame>, KissError> {
        loop {
            // Find the first FEND.
            let Some(first) = self.buffer.iter().position(|&b| b == FEND) else {
                // No FEND at all — nothing to decode yet.
                return Ok(None);
            };
            if !self.in_frame {
                // Discard any pre-FEND garbage and start a new frame.
                drop(self.buffer.drain(..first));
                self.in_frame = true;
            }
            // We're now positioned with a leading FEND at buffer[0].
            // Look for the next FEND to close the frame.
            let tail = self.buffer.get(1..).unwrap_or(&[][..]);
            let Some(end) = tail.iter().position(|&b| b == FEND) else {
                return Ok(None);
            };
            let end_idx = end + 1;
            // Empty frame (`FEND FEND`)? Skip and stay in-frame.
            if end_idx == 1 {
                drop(self.buffer.drain(..1));
                continue;
            }
            // Slice the complete frame including both FENDs.
            let frame_bytes: Vec<u8> = self.buffer.get(..=end_idx).unwrap_or(&[][..]).to_vec();
            drop(self.buffer.drain(..=end_idx));
            self.in_frame = false;
            return decode_kiss_frame(&frame_bytes).map(Some);
        }
    }
}
