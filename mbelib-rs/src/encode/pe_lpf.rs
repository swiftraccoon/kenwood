// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder/pe_lpf.cc)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! 21-tap symmetric FIR lowpass filter for pitch estimation.
//!
//! Port of `imbe_vocoder::pe_lpf()` from OP25. The coefficients are
//! from the reference implementation and are symmetric around the
//! center tap, giving the filter linear phase — required so pitch
//! estimation sees undistorted waveform periodicity.
//!
//! The original uses ETSI Q1.15 fixed-point (`mac` into a Q1.31
//! accumulator, then `round()` back to Q1.15). We keep the same
//! coefficient values scaled by 1/32768 to match the filter's
//! frequency response exactly.

use crate::encode::state::PE_LPF_ORD;

/// 21-tap symmetric FIR coefficients (Q1.15 values / 32768.0).
///
/// Verbatim from Yazev's `pe_lpf.cc`. The filter has center tap at
/// index 10 (11512 / 32768 ≈ 0.3513) with a classic linear-phase
/// symmetric structure. Low-pass corner is roughly 800 Hz at 8 kHz
/// sample rate, which pulls pitch harmonics clear of formant
/// structure for cleaner period estimation.
const LPF_COEF: [f32; PE_LPF_ORD] = [
    -94.0 / 32768.0,
    -92.0 / 32768.0,
    185.0 / 32768.0,
    543.0 / 32768.0,
    288.0 / 32768.0,
    -883.0 / 32768.0,
    -1834.0 / 32768.0,
    -495.0 / 32768.0,
    3891.0 / 32768.0,
    9141.0 / 32768.0,
    11512.0 / 32768.0,
    9141.0 / 32768.0,
    3891.0 / 32768.0,
    -495.0 / 32768.0,
    -1834.0 / 32768.0,
    -883.0 / 32768.0,
    288.0 / 32768.0,
    543.0 / 32768.0,
    185.0 / 32768.0,
    -92.0 / 32768.0,
    -94.0 / 32768.0,
];

/// Convolve `sigin` with the 21-tap LPF, carrying state in `mem`.
///
/// Algorithm mirrors the reference: slide the memory one sample left
/// per input, append the new sample, convolve with coefficients,
/// write the result. `mem.len()` must equal [`PE_LPF_ORD`]; the
/// public caller constructs this from [`EncoderBuffers::pe_lpf_mem`].
///
/// [`EncoderBuffers::pe_lpf_mem`]: crate::encode::state::EncoderBuffers
pub(crate) fn pe_lpf(sigin: &[f32], sigout: &mut [f32], mem: &mut [f32; PE_LPF_ORD]) {
    for (&x, y) in sigin.iter().zip(sigout.iter_mut()) {
        // Shift delay line left one step; append new sample at the
        // right. `copy_within` handles the overlap safely.
        mem.copy_within(1..PE_LPF_ORD, 0);
        if let Some(last) = mem.last_mut() {
            *last = x;
        }
        // Convolution sum.
        let mut sum = 0.0_f32;
        for (&m, &c) in mem.iter().zip(LPF_COEF.iter()) {
            sum += m * c;
        }
        *y = sum;
    }
}

#[cfg(test)]
mod tests {
    use super::{LPF_COEF, pe_lpf};
    use crate::encode::state::PE_LPF_ORD;

    /// Filter coefficients must be symmetric (linear phase).
    #[test]
    fn coefficients_are_symmetric() {
        for i in 0..PE_LPF_ORD / 2 {
            let left = LPF_COEF[i];
            let right = LPF_COEF[PE_LPF_ORD - 1 - i];
            assert!(
                (left - right).abs() < 1e-9,
                "asymmetry at tap {i}: {left} vs {right}",
            );
        }
    }

    /// A DC input (1.0) eventually saturates to the sum of all
    /// coefficients — which is ~0.75 for this filter.
    #[test]
    fn dc_gain_matches_coefficient_sum() {
        let input = [1.0_f32; 200];
        let mut output = [0.0_f32; 200];
        let mut mem = [0.0_f32; PE_LPF_ORD];
        pe_lpf(&input, &mut output, &mut mem);
        let expected_gain: f32 = LPF_COEF.iter().sum();
        // After the filter fills (~21 samples), output is stable.
        let measured = output[100];
        assert!(
            (measured - expected_gain).abs() < 1e-5,
            "DC gain {measured} vs expected {expected_gain}",
        );
    }

    /// Impulse response is just the coefficients themselves, in
    /// order, delayed by the filter's settling. We pass a single 1.0
    /// followed by zeros and verify the output equals the (reversed
    /// because of the right-shift appending) coefficient set.
    #[test]
    fn impulse_response_matches_coefficients() {
        let mut input = [0.0_f32; PE_LPF_ORD + 4];
        input[0] = 1.0;
        let mut output = [0.0_f32; PE_LPF_ORD + 4];
        let mut mem = [0.0_f32; PE_LPF_ORD];
        pe_lpf(&input, &mut output, &mut mem);
        // Because the delay line appends the new sample at the right
        // and convolves against LPF_COEF[0..21] in order, the
        // impulse appears at output position 0 multiplied by
        // LPF_COEF[PE_LPF_ORD - 1], then drifts left through the
        // coefficients as it moves through the line. So output[k]
        // for k < PE_LPF_ORD equals LPF_COEF[PE_LPF_ORD - 1 - k].
        for (k, &out) in output.iter().enumerate().take(PE_LPF_ORD) {
            let expected = LPF_COEF[PE_LPF_ORD - 1 - k];
            assert!(
                (out - expected).abs() < 1e-9,
                "impulse mismatch at k={k}: {out} vs {expected}",
            );
        }
    }
}
