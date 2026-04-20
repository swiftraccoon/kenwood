// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Direct-form-I biquad runner for the filter banks in
// [`crate::encode::kenwood::biquads`]. The COEFFICIENTS are lifted
// from the TH-D75 firmware verbatim; this file owns the ARITHMETIC
// (add / multiply / update delay line) that consumes them. Keeping
// the two separated preserves the "data only" provenance of the
// sibling `biquads.rs` file without scattering filter-runner code
// elsewhere in the encoder.

//! Stateful biquad filters for the Kenwood coefficient banks.
//!
//! A direct-form-I biquad implements:
//!
//! ```text
//! y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]
//! ```
//!
//! The coefficient layout in [`crate::encode::kenwood::biquads`] is
//! `[b0, b1, b2, a1, a2]` with the standard DSP convention
//! (`H(z) = B(z) / (1 + a1·z⁻¹ + a2·z⁻²)`, feedback terms
//! SUBTRACTED from the new output). Despite the header comment in
//! the `rust_integration/` crate calling the tail "-a1, -a2", the
//! actual stored values are canonical: stability analysis of the
//! 345 Hz HPF row 0
//! (`[0.98168, −1.96336, 0.98168, −1.96302, 0.96370]`) shows the
//! pole pair at `z = 0.982 ± 0.019i` only when the last two values
//! are treated as `a1, a2` directly (difference-equation subtracts
//! them). Treating them as "pre-negated" puts a pole at `z ≈ −2.37`
//! which explodes on the first DC input.

/// Per-stream delay-line state for one biquad section.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Biquad2State {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad2State {
    /// Fresh state, all four history taps zero.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
}

/// Run one biquad section over an input block, writing to the output
/// block in place. `coeffs` is `[b0, b1, b2, a1, a2]` — the last two
/// are the feedback coefficients and are SUBTRACTED from the new
/// output per the standard DF-I convention. `state` carries the
/// four-tap delay line across calls.
///
/// `input` and `output` may alias; the reads from `input[i]` happen
/// before the write to `output[i]`, so `sigout == sigin` is safe.
#[inline]
pub(crate) fn biquad_df1_section(
    coeffs: &[f32; 5],
    input: &[f32],
    output: &mut [f32],
    state: &mut Biquad2State,
) {
    let [b0, b1, b2, a1, a2] = *coeffs;
    // Precompute the negated feedback coefficients so the inner
    // loop's fused multiply-add chain stays additive (`mul_add` is
    // one instruction per term on modern CPUs).
    let minus_a1 = -a1;
    let minus_a2 = -a2;
    for (i, y_slot) in output.iter_mut().enumerate() {
        let Some(&x0) = input.get(i) else {
            break;
        };
        let y0 = b0.mul_add(
            x0,
            b1.mul_add(
                state.x1,
                b2.mul_add(state.x2, minus_a1.mul_add(state.y1, minus_a2 * state.y2)),
            ),
        );
        state.x2 = state.x1;
        state.x1 = x0;
        state.y2 = state.y1;
        state.y1 = y0;
        *y_slot = y0;
    }
}

#[cfg(test)]
mod tests {
    use super::{Biquad2State, biquad_df1_section};
    use crate::encode::kenwood::biquads::HPF_345HZ_COEFFS;

    /// DC input decays to zero through the 345 Hz HPF: the filter's
    /// zeros sit on the unit circle at DC, so steady-state gain at
    /// ω=0 is mathematically zero.
    #[test]
    fn hpf_345hz_kills_dc() {
        let mut state = Biquad2State::new();
        let input = [1.0_f32; 2000];
        let mut output = [0.0_f32; 2000];
        biquad_df1_section(&HPF_345HZ_COEFFS, &input, &mut output, &mut state);
        let tail_max = output
            .iter()
            .skip(1500)
            .map(|&v| v.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            tail_max < 1e-3,
            "DC should decay to near-zero; tail_max={tail_max}"
        );
    }

    /// A 1 kHz tone passes through the 345 Hz HPF essentially
    /// intact (at 1 kHz we're well above the corner, gain should be
    /// close to unity).
    #[test]
    fn hpf_345hz_passes_1khz_tone() {
        let mut state = Biquad2State::new();
        let input: Vec<f32> = (0..4000)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32;
                (t * 2.0 * std::f32::consts::PI * 1000.0 / 8000.0).sin()
            })
            .collect();
        let mut output = vec![0.0_f32; input.len()];
        biquad_df1_section(&HPF_345HZ_COEFFS, &input, &mut output, &mut state);
        // Skip first 200 samples to let the transient die.
        let rms_in = rms(&input[200..]);
        let rms_out = rms(&output[200..]);
        let ratio = rms_out / rms_in;
        assert!(
            (0.85..1.15).contains(&ratio),
            "1 kHz tone should pass through ~intact; rms ratio = {ratio}"
        );
    }

    /// The filter's pole pair sits very close to its double zero at
    /// DC, so the notch is narrow. At 100 Hz — well above the
    /// effective cutoff — the signal passes through nearly intact.
    /// Documents Kenwood's actual design (DC trap, not sub-corner
    /// attenuator).
    #[test]
    fn hpf_100hz_passes_nearly_intact() {
        let mut state = Biquad2State::new();
        let input: Vec<f32> = (0..4000)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32;
                (t * 2.0 * std::f32::consts::PI * 100.0 / 8000.0).sin()
            })
            .collect();
        let mut output = vec![0.0_f32; input.len()];
        biquad_df1_section(&HPF_345HZ_COEFFS, &input, &mut output, &mut state);
        let rms_in = rms(&input[200..]);
        let rms_out = rms(&output[200..]);
        let ratio = rms_out / rms_in;
        assert!(
            (0.9..1.1).contains(&ratio),
            "100 Hz tone should pass through; got ratio {ratio}"
        );
    }

    /// A true sub-audible rumble at 5 Hz IS attenuated, because 5 Hz
    /// falls inside the narrow notch around DC.
    #[test]
    fn hpf_attenuates_sub_10hz_rumble() {
        let mut state = Biquad2State::new();
        let input: Vec<f32> = (0..16000)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32;
                (t * 2.0 * std::f32::consts::PI * 5.0 / 8000.0).sin()
            })
            .collect();
        let mut output = vec![0.0_f32; input.len()];
        biquad_df1_section(&HPF_345HZ_COEFFS, &input, &mut output, &mut state);
        let rms_in = rms(&input[4000..]);
        let rms_out = rms(&output[4000..]);
        let ratio = rms_out / rms_in;
        assert!(
            ratio < 0.5,
            "5 Hz rumble should be attenuated >6 dB; got ratio {ratio}"
        );
    }

    fn rms(xs: &[f32]) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let n = xs.len() as f32;
        (xs.iter().map(|x| x * x).sum::<f32>() / n).sqrt()
    }
}
