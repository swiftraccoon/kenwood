// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Learned waveform enhancement (complex-STFT masking).
//!
//! Blind listening established that the audible distance between
//! this decoder and reference hardware-grade decodes of identical
//! AMBE frames lives in waveform fine structure — magnitude-only
//! post-processing is imperceptible. This module runs a small
//! convolutional network that predicts a bounded complex mask
//! (magnitude *and* phase corrections around identity) over the
//! decoder output's STFT, ports of the training-side model with
//! embedded weights. Offline/whole-clip processing; the network is
//! non-causal over a ±few-frame horizon.
//!
//! The forward pass reproduces the training framework's semantics —
//! centered reflect-padded STFT (256/64, Hann), exact-erf GELU,
//! zero-padded 3×3 convolutions (one layer time-dilated), and
//! window-envelope-normalized inverse STFT — and is pinned against a
//! recorded reference vector produced by the training checkpoint.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// STFT size (32 ms at 8 kHz).
const N_FFT: usize = 256;
/// STFT hop (8 ms at 8 kHz).
const HOP: usize = 64;
/// One-sided spectrum bins.
const BINS: usize = N_FFT / 2 + 1;
/// Conv channel width.
const CH: usize = 48;

/// Embedded network weights, exported from the training checkpoint
/// (layer order and shapes in `model.layout.txt`).
static MODEL_BIN: &[u8] = include_bytes!("model.bin");

/// One zero-padded 3×3 convolution layer's parameters.
#[derive(Debug)]
struct Conv {
    weight: Vec<f32>, // [out][in][3][3] C-contiguous
    bias: Vec<f32>,
    out_ch: usize,
    in_ch: usize,
    /// Time-axis dilation (1 or 2); padding matches for same-size output.
    dilation_t: usize,
}

/// The five-layer masking network.
#[derive(Debug)]
pub struct WaveEnhancer {
    layers: Vec<Conv>,
    window: [f32; N_FFT],
}

/// Failures constructing the enhancer.
#[derive(Debug)]
pub enum WaveEnhanceError {
    /// The embedded weight blob does not match the expected layout.
    BadBlob(usize),
}

impl std::fmt::Display for WaveEnhanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadBlob(size) => {
                write!(f, "embedded model blob has unexpected size: {size} bytes")
            }
        }
    }
}

impl std::error::Error for WaveEnhanceError {}

fn read_f32s(blob: &[u8], offset: &mut usize, count: usize) -> Option<Vec<f32>> {
    let bytes = blob.get(*offset..*offset + count * 4)?;
    *offset += count * 4;
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
            .collect(),
    )
}

/// Exact-erf GELU, matching the training framework's default.
/// erf via Abramowitz & Stegun 7.1.26 (|error| ≤ 1.5e-7).
fn gelu(x: f32) -> f32 {
    let z = x / std::f32::consts::SQRT_2;
    let t = 1.0 / z.abs().mul_add(0.327_591_1, 1.0);
    let poly = t * t.mul_add(
        t.mul_add(
            t.mul_add(t.mul_add(1.061_405_4, -1.453_152_1), 1.421_413_7),
            -0.284_496_74,
        ),
        0.254_829_6,
    );
    let erf_abs = 1.0 - poly * (-z * z).exp();
    let erf = if z < 0.0 { -erf_abs } else { erf_abs };
    0.5 * x * (1.0 + erf)
}

impl WaveEnhancer {
    /// Parse the embedded weights.
    ///
    /// # Errors
    ///
    /// [`WaveEnhanceError::BadBlob`] when the embedded blob size does
    /// not match the compiled-in layer layout.
    pub fn new() -> Result<Self, WaveEnhanceError> {
        // (out, in, dilation_t) per layer, mirroring the exporter.
        let shapes = [
            (CH, 3, 1),
            (CH, CH, 1),
            (CH, CH, 1),
            (CH, CH, 2),
            (2, CH, 1),
        ];
        let expected: usize = shapes.iter().map(|&(o, i, _)| o * i * 9 + o).sum::<usize>() * 4;
        if MODEL_BIN.len() != expected {
            return Err(WaveEnhanceError::BadBlob(MODEL_BIN.len()));
        }
        let mut offset = 0usize;
        let mut layers = Vec::with_capacity(shapes.len());
        for &(out_ch, in_ch, dilation_t) in &shapes {
            let weight = read_f32s(MODEL_BIN, &mut offset, out_ch * in_ch * 9)
                .ok_or(WaveEnhanceError::BadBlob(MODEL_BIN.len()))?;
            let bias = read_f32s(MODEL_BIN, &mut offset, out_ch)
                .ok_or(WaveEnhanceError::BadBlob(MODEL_BIN.len()))?;
            layers.push(Conv {
                weight,
                bias,
                out_ch,
                in_ch,
                dilation_t,
            });
        }
        let mut window = [0.0_f32; N_FFT];
        for (n, slot) in window.iter_mut().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "n < 256; exact in f32")]
            let phase = std::f32::consts::TAU * n as f32 / N_FFT as f32;
            *slot = 0.5_f32.mul_add(-phase.cos(), 0.5);
        }
        Ok(Self { layers, window })
    }

    /// Enhance one whole clip of decoder output PCM.
    ///
    /// Input and output are the decoder's native 8 kHz mono stream;
    /// the length is preserved. Clips shorter than a few frames pass
    /// through unchanged.
    #[must_use]
    pub fn process(&self, pcm: &[i16]) -> Vec<i16> {
        if pcm.len() < N_FFT * 2 {
            return pcm.to_vec();
        }
        let x: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32_768.0).collect();
        let y = self.process_f32(&x);
        y.iter()
            .map(|&v| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "clamped to i16 range on the previous line"
                )]
                let s = (v * 32_768.0).clamp(-32_767.0, 32_767.0) as i16;
                s
            })
            .collect()
    }

    /// Float-domain processing (unit-scale samples). The parity test
    /// drives this directly against the training checkpoint's output.
    #[must_use]
    pub fn process_f32(&self, x: &[f32]) -> Vec<f32> {
        // Centered STFT: reflect-pad n_fft/2 on both ends.
        let pad = N_FFT / 2;
        let mut padded = Vec::with_capacity(x.len() + 2 * pad);
        for i in (1..=pad).rev() {
            padded.push(x.get(i).copied().unwrap_or(0.0));
        }
        padded.extend_from_slice(x);
        for i in 1..=pad {
            padded.push(x.get(x.len().wrapping_sub(1 + i)).copied().unwrap_or(0.0));
        }
        let frames = (padded.len() - N_FFT) / HOP + 1;

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N_FFT);
        let ifft = planner.plan_fft_inverse(N_FFT);
        let mut spec = vec![Complex::new(0.0_f32, 0.0); frames * BINS];
        let mut buf = fft.make_input_vec();
        let mut freq = fft.make_output_vec();
        let mut scratch = fft.make_scratch_vec();
        for f in 0..frames {
            for (n, slot) in buf.iter_mut().enumerate() {
                *slot = padded.get(f * HOP + n).copied().unwrap_or(0.0)
                    * self.window.get(n).copied().unwrap_or(0.0);
            }
            let _ = fft.process_with_scratch(&mut buf, &mut freq, &mut scratch);
            if let Some(dst) = spec.get_mut(f * BINS..(f + 1) * BINS) {
                dst.copy_from_slice(&freq);
            }
        }

        // Features: [logmag, cos, sin] × BINS × frames.
        let mut feat = vec![0.0_f32; 3 * BINS * frames];
        for f in 0..frames {
            for b in 0..BINS {
                let c = spec.get(f * BINS + b).copied().unwrap_or_default();
                let mag = c.norm().max(1e-6);
                let idx = b * frames + f;
                if let Some(s) = feat.get_mut(idx) {
                    *s = mag.ln();
                }
                if let Some(s) = feat.get_mut(BINS * frames + idx) {
                    *s = c.re / mag;
                }
                if let Some(s) = feat.get_mut(2 * BINS * frames + idx) {
                    *s = c.im / mag;
                }
            }
        }

        // Conv stack (channels-major planes of BINS×frames).
        let mut cur = feat;
        for (li, layer) in self.layers.iter().enumerate() {
            let mut next = vec![0.0_f32; layer.out_ch * BINS * frames];
            conv3x3(&cur, &mut next, layer, BINS, frames);
            if li + 1 < self.layers.len() {
                for v in &mut next {
                    *v = gelu(*v);
                }
            }
            cur = next;
        }

        // Apply mask: (1 + tanh(m0)) + i·tanh(m1); inverse STFT.
        let mut out_padded = vec![0.0_f32; padded.len()];
        let mut norm = vec![0.0_f32; padded.len()];
        let mut time = ifft.make_output_vec();
        let mut freq_in = ifft.make_input_vec();
        let mut iscratch = ifft.make_scratch_vec();
        #[expect(clippy::cast_precision_loss, reason = "N_FFT is 256; exact in f32")]
        let inv_n = 1.0 / N_FFT as f32;
        for f in 0..frames {
            for b in 0..BINS {
                let idx = b * frames + f;
                let m0 = cur.get(idx).copied().unwrap_or(0.0);
                let m1 = cur.get(BINS * frames + idx).copied().unwrap_or(0.0);
                let mask = Complex::new(1.0 + m0.tanh(), m1.tanh());
                let c = spec.get(f * BINS + b).copied().unwrap_or_default();
                if let Some(slot) = freq_in.get_mut(b) {
                    *slot = c * mask;
                }
            }
            let _ = ifft.process_with_scratch(&mut freq_in, &mut time, &mut iscratch);
            for (n, &v) in time.iter().enumerate() {
                let w = self.window.get(n).copied().unwrap_or(0.0);
                if let Some(slot) = out_padded.get_mut(f * HOP + n) {
                    *slot += v * inv_n * w;
                }
                if let Some(slot) = norm.get_mut(f * HOP + n) {
                    *slot += w * w;
                }
            }
        }
        (0..x.len())
            .map(|i| {
                let num = out_padded.get(pad + i).copied().unwrap_or(0.0);
                let den = norm.get(pad + i).copied().unwrap_or(1.0).max(1e-8);
                num / den
            })
            .collect()
    }
}

/// Zero-padded 3×3 convolution over (freq, time) planes, with a
/// time-axis dilation of 1 or 2 (padding matches the dilation so the
/// output keeps the input size).
fn conv3x3(input: &[f32], output: &mut [f32], layer: &Conv, height: usize, width: usize) {
    let dt = layer.dilation_t;
    for oc in 0..layer.out_ch {
        let bias = layer.bias.get(oc).copied().unwrap_or(0.0);
        for b in 0..height {
            for f in 0..width {
                let mut acc = bias;
                for ic in 0..layer.in_ch {
                    let plane = ic * height * width;
                    let wbase = (oc * layer.in_ch + ic) * 9;
                    for (kb, brow) in [-1_isize, 0, 1].into_iter().enumerate() {
                        #[expect(clippy::cast_possible_wrap, reason = "height ≤ 129, fits isize")]
                        let bb = b as isize + brow;
                        if bb < 0 || bb >= height.cast_signed() {
                            continue;
                        }
                        for (kf, frow) in [-1_isize, 0, 1].into_iter().enumerate() {
                            #[expect(clippy::cast_possible_wrap, reason = "frame count fits isize")]
                            let ff = f as isize + frow * dt as isize;
                            if ff < 0 || ff >= width.cast_signed() {
                                continue;
                            }
                            #[expect(
                                clippy::cast_sign_loss,
                                reason = "bounds-checked non-negative above"
                            )]
                            let src = plane + bb as usize * width + ff as usize;
                            let w = layer
                                .weight
                                .get(wbase + kb * 3 + kf)
                                .copied()
                                .unwrap_or(0.0);
                            acc = input.get(src).copied().unwrap_or(0.0).mul_add(w, acc);
                        }
                    }
                }
                if let Some(slot) = output.get_mut(oc * height * width + b * width + f) {
                    *slot = acc;
                }
            }
        }
    }
}
