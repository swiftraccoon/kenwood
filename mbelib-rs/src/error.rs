// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Caller-constructed diagnostics for AMBE FEC correction counts.
//!
//! [`crate::frame_fec`] returns correction counts and frame classification;
//! [`crate::AmbeDecoder::decode_frame`] returns PCM directly. This diagnostic
//! type stores counts supplied by a caller that selects an error threshold.

use core::fmt;

/// An excessive-correction diagnostic with C0 and total correction counts.
///
/// The counts describe the caller's reported FEC corrections in bits. They are
/// observations supplied when constructing the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Correction counts that exceed the caller's reliability threshold.
    ExcessiveErrors {
        /// Reported corrections in the C0 codeword, in bits.
        c0_errors: u32,
        /// Reported total FEC corrections, in bits.
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
