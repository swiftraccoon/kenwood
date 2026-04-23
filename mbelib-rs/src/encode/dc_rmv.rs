// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder/dc_rmv.cc)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! DC-removal high-pass filter.
//!
//! Port of `imbe_vocoder::dc_rmv()` from OP25. Algorithm is a simple
//! first-order IIR:
//!
//! ```text
//!     y[n] = x[n] + state[n-1]
//!     state[n] = 0.99 * y[n] - x[n]
//! ```
//!
//! With α = 0.99, the −3 dB corner sits at about 13 Hz, well below the
//! 250–500 Hz lower edge of human speech — so audible content passes
//! unattenuated and the integrator quickly eliminates any DC bias or
//! very-low-frequency microphone artifacts.

/// High-pass coefficient (Yazev's `CNST_0_99_Q1_15` = 0x7EB8 / 32768
/// ≈ 0.99005).
const ALPHA: f32 = 0.990_05;

/// Apply the DC-removal high-pass in place.
///
/// `sigin` is the block of new samples (length `len`). Output is
/// written to `sigout` (same length). `mem` carries the integrator
/// state across calls — zero at stream start, updated on return.
///
/// Panics if `sigin.len()` and `sigout.len()` are both less than the
/// advertised frame size (we iterate over `min(sigin.len(), sigout.len())`).
pub(crate) fn dc_rmv(sigin: &[f32], sigout: &mut [f32], mem: &mut f32) {
    let mut state = *mem;
    for (&x, y) in sigin.iter().zip(sigout.iter_mut()) {
        let sum = state + x;
        *y = sum;
        // `mul_add` delivers one fused multiply-add instruction on
        // modern CPUs and is more accurate than separate `*` and `-`.
        state = ALPHA.mul_add(sum, -x);
    }
    *mem = state;
}

#[cfg(test)]
mod tests {
    use super::dc_rmv;

    /// DC input → zero output at steady state. First sample of DC
    /// passes through; integrator then subtracts it on subsequent
    /// samples, driving output to zero.
    #[test]
    fn dc_input_decays_to_zero() {
        let input = [1.0_f32; 2000];
        let mut output = [0.0_f32; 2000];
        let mut mem = 0.0;
        dc_rmv(&input, &mut output, &mut mem);
        // After ~500 samples (~62 ms at 8 kHz) the output should be
        // well below 1% of the input DC level. 0.99^500 ≈ 0.0066.
        let tail_max = output[1500..]
            .iter()
            .map(|&v| v.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            tail_max < 0.01,
            "DC should decay to near-zero; tail_max={tail_max}",
        );
    }

    /// An AC signal centered around zero passes through essentially
    /// unchanged (the HPF notch is far below speech bands).
    #[test]
    fn ac_input_passes_through() {
        // Simulated 500 Hz sine at 8 kHz sample rate, 1000 samples.
        let input: Vec<f32> = (0..1000)
            .map(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test tone generator: i < 1000 is well within f32 mantissa."
                )]
                let t = i as f32;
                (t * 2.0 * std::f32::consts::PI * 500.0 / 8000.0).sin()
            })
            .collect();
        let mut output = vec![0.0_f32; 1000];
        let mut mem = 0.0;
        dc_rmv(&input, &mut output, &mut mem);
        // Compare RMS: output should retain most of the input energy.
        let rms_in = rms(&input);
        let rms_out = rms(&output[100..]); // skip transient
        let ratio = rms_out / rms_in;
        assert!(
            ratio > 0.95,
            "500 Hz sine should pass through; rms ratio = {ratio}",
        );
    }

    fn rms(xs: &[f32]) -> f32 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test RMS helper: xs.len() = 1000 for this test, exact in f32."
        )]
        let n = xs.len() as f32;
        (xs.iter().map(|x| x * x).sum::<f32>() / n).sqrt()
    }
}
