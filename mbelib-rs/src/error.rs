//! Error types for the AMBE 3600×2450 decoder.
//!
//! The AMBE codec uses Golay(23,12) and Hamming(15,11) forward error
//! correction to protect the parameter bits. When the channel
//! introduces more errors than the FEC can correct, the decoder
//! detects this via syndrome analysis and reports it here.

use core::fmt;

/// Errors that can occur during AMBE frame decoding.
///
/// These errors represent conditions where the codec cannot produce
/// reliable audio output. The decoder handles them internally by
/// repeating the previous frame's parameters (up to 3 times) and
/// then muting to silence. Callers generally do not need to inspect
/// these — [`AmbeDecoder::decode_frame`](crate::AmbeDecoder::decode_frame)
/// always returns a valid PCM buffer, using silence as the fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Too many bit errors for the FEC to correct.
    ///
    /// The Golay and Hamming decoders detected more errors than their
    /// correction capacity (3 bits for Golay, 1 bit for Hamming). The
    /// decoded parameters are unreliable and should not be used for
    /// synthesis.
    ExcessiveErrors {
        /// Errors detected in the C0 codeword (Golay-protected).
        ///
        /// C0 carries the fundamental frequency index (b0), the most
        /// critical parameter. Even 1 error here can shift the pitch
        /// drastically.
        c0_errors: u32,
        /// Total errors across all four codewords (C0–C3).
        total_errors: u32,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExcessiveErrors {
                c0_errors,
                total_errors,
            } => {
                write!(
                    f,
                    "excessive AMBE bit errors: {c0_errors} in C0, {total_errors} total"
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}
