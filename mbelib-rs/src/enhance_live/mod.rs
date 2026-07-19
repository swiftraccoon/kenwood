// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Learned waveform enhancement, causal/streaming variant
//! (complex-STFT masking with a forward-only recurrence).
//!
//! Same architecture family as [`crate::enhance_wave`] — a small
//! grouped-convolution network predicting a bounded complex mask
//! (magnitude *and* phase corrections around identity) over the
//! decoder output's STFT — but the bidirectional recurrent core is
//! replaced by a single *forward* GRU, so the recurrence never reads
//! future spectral columns. The only remaining lookahead is the
//! convolutions' short time context, which makes frame-in/frame-out
//! streaming possible.
//!
//! Entry points:
//!
//! - [`LiveWaveEnhancer`] — whole-clip batch processing with the same
//!   semantics as the offline enhancer (centered reflect-padded STFT,
//!   256/64, Hann; exact-erf GELU; the training framework's GRU gate
//!   order; window-normalized inverse STFT), pinned against a
//!   recorded reference vector produced by the training checkpoint.
//! - [`LiveWaveStream`] — incremental processing that reproduces the
//!   batch output while emitting each sample as soon as it is final.
//!
//! # Latency budget
//!
//! A streamed output sample is final at most 447 input samples (just
//! under 56 ms at 8 kHz) after the matching input sample arrives:
//!
//! - **128 samples (16 ms)** — centered-STFT offset: each analysis
//!   window extends half a window past its column center. (The left
//!   half-window is covered by mirroring the first 128 samples as
//!   reflect padding, so stream start pays no extra delay.)
//! - **192 samples (24 ms)** — convolutional lookahead: the two
//!   grouped frequency stages and the mask head each see one STFT
//!   column (64 samples) ahead, three hops end to end.
//! - **127 samples (just under 16 ms)** — overlap-add completion: a
//!   sample is emitted only after the last analysis window
//!   overlapping it has been masked, inverse-transformed, and
//!   accumulated together with its squared-window normalization.
//!
//! Additionally, nothing is released until the stream has seen 512
//! input samples (64 ms) — the batch API's short-clip passthrough
//! threshold — which delays only the first release, not steady-state
//! pacing. The stream is designed to sit inside a receiver playout
//! buffer that already holds at least that much audio, where the
//! enhancement adds no end-to-end delay of its own;
//! [`LiveWaveStream::finish`] drains the residual lookahead at end of
//! transmission so total output length always equals total input
//! length.

use std::collections::VecDeque;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// STFT size (32 ms at 8 kHz).
const N_FFT: usize = 256;
/// STFT hop (8 ms at 8 kHz).
const HOP: usize = 64;
/// One-sided spectrum bins.
const BINS: usize = N_FFT / 2 + 1;
/// Convolution channel width.
const CH: usize = 56;
/// Grouped-conv group count.
const GROUPS: usize = 4;
/// Downsampled channel count feeding the recurrence.
const DCH: usize = 16;
/// Downsampled frequency bins feeding the recurrence.
const DFR: usize = 32;
/// Recurrent input width (`DCH * DFR`).
const GRU_IN: usize = DCH * DFR;
/// Recurrent hidden width (single forward direction).
const GRU_H: usize = 256;
/// Centered-STFT reflect padding on each side (`N_FFT / 2`).
const PAD: usize = N_FFT / 2;
/// Network time lookahead in STFT columns: `freq1`, `freq2`, and the
/// mask head each see one column ahead.
const LOOKAHEAD_COLS: usize = 3;
/// Input samples a stream must see before releasing output — the
/// batch API's short-clip passthrough threshold.
const RELEASE_MIN: usize = N_FFT * 2;
/// Inverse-FFT normalization.
#[expect(clippy::cast_precision_loss, reason = "N_FFT is 256; exact in f32")]
const INV_N: f32 = 1.0 / (N_FFT as f32);

/// Embedded network weights, exported from the training checkpoint
/// (layer order and shapes in `model.layout.txt`).
static MODEL_BIN: &[u8] = include_bytes!("model.bin");

/// One 2-D convolution's parameters and geometry.
#[derive(Debug)]
struct Conv {
    weight: Vec<f32>, // [out][in/groups][kh][kw] C-contiguous
    bias: Vec<f32>,
    out_c: usize,
    in_c: usize,
    kh: usize,
    kw: usize,
    sh: usize,
    sw: usize,
    ph: usize,
    pw: usize,
    groups: usize,
}

/// The forward GRU's parameters (gate order: reset, update, new).
#[derive(Debug)]
struct GruDir {
    w_ih: Vec<f32>, // [3*GRU_H][GRU_IN]
    w_hh: Vec<f32>, // [3*GRU_H][GRU_H]
    b_ih: Vec<f32>,
    b_hh: Vec<f32>,
}

/// The full masking network — weights and analysis window — shared by
/// the batch and streaming paths.
#[derive(Debug)]
struct Model {
    inp: Conv,
    freq1: Conv,
    freq2: Conv,
    down: Conv,
    gru: GruDir,
    up_w: Vec<f32>, // [GRU_IN][GRU_H]
    up_b: Vec<f32>,
    mix: Conv,
    out: Conv,
    window: [f32; N_FFT],
}

/// Failures constructing the live enhancer.
#[derive(Debug)]
pub enum LiveWaveEnhanceError {
    /// The embedded weight blob does not match the expected layout.
    BadBlob(usize),
}

impl std::fmt::Display for LiveWaveEnhanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadBlob(size) => {
                write!(
                    f,
                    "embedded live model blob has unexpected size: {size} bytes"
                )
            }
        }
    }
}

impl std::error::Error for LiveWaveEnhanceError {}

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

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Dense `y = W·x + b` for row-major `W` of `rows × cols`.
fn matvec(w: &[f32], b: &[f32], x: &[f32], rows: usize, cols: usize, y: &mut [f32]) {
    for r in 0..rows {
        let mut acc = b.get(r).copied().unwrap_or(0.0);
        let row = w.get(r * cols..(r + 1) * cols).unwrap_or(&[]);
        for (wv, xv) in row.iter().zip(x.iter()) {
            acc = wv.mul_add(*xv, acc);
        }
        if let Some(slot) = y.get_mut(r) {
            *slot = acc;
        }
    }
}

/// Grouped, strided, zero-padded 2-D convolution over channel-major
/// `(channels, height, width)` planes.
fn conv2d(input: &[f32], h: usize, w: usize, layer: &Conv) -> (Vec<f32>, usize, usize) {
    let h_out = (h + 2 * layer.ph - layer.kh) / layer.sh + 1;
    let w_out = (w + 2 * layer.pw - layer.kw) / layer.sw + 1;
    let in_per_g = layer.in_c / layer.groups;
    let out_per_g = layer.out_c / layer.groups;
    let mut output = vec![0.0_f32; layer.out_c * h_out * w_out];
    for oc in 0..layer.out_c {
        let g = oc / out_per_g;
        let bias = layer.bias.get(oc).copied().unwrap_or(0.0);
        for oy in 0..h_out {
            for ox in 0..w_out {
                let mut acc = bias;
                for icg in 0..in_per_g {
                    let ic = g * in_per_g + icg;
                    let plane = ic * h * w;
                    let wbase = (oc * in_per_g + icg) * layer.kh * layer.kw;
                    for ky in 0..layer.kh {
                        let iy = oy * layer.sh + ky;
                        if iy < layer.ph {
                            continue;
                        }
                        let iy = iy - layer.ph;
                        if iy >= h {
                            continue;
                        }
                        for kx in 0..layer.kw {
                            let ix = ox * layer.sw + kx;
                            if ix < layer.pw {
                                continue;
                            }
                            let ix = ix - layer.pw;
                            if ix >= w {
                                continue;
                            }
                            let wv = layer
                                .weight
                                .get(wbase + ky * layer.kw + kx)
                                .copied()
                                .unwrap_or(0.0);
                            acc = input
                                .get(plane + iy * w + ix)
                                .copied()
                                .unwrap_or(0.0)
                                .mul_add(wv, acc);
                        }
                    }
                }
                if let Some(slot) = output.get_mut(oc * h_out * w_out + oy * w_out + ox) {
                    *slot = acc;
                }
            }
        }
    }
    (output, h_out, w_out)
}

/// One time column of [`conv2d`]: identical accumulation order (bias,
/// then per-group input channel, kernel row, kernel column), with
/// absent neighbour columns skipped exactly as the batch path skips
/// its zero time padding. `cols` holds `layer.kw` entries, the input
/// columns at times `t - pw .. t - pw + kw`; each is a channel-major
/// `[in_c][h]` slice.
fn conv_col(layer: &Conv, cols: &[Option<&[f32]>], h: usize, out: &mut [f32]) {
    let h_out = (h + 2 * layer.ph - layer.kh) / layer.sh + 1;
    let in_per_g = layer.in_c / layer.groups;
    let out_per_g = layer.out_c / layer.groups;
    for oc in 0..layer.out_c {
        let g = oc / out_per_g;
        let bias = layer.bias.get(oc).copied().unwrap_or(0.0);
        for oy in 0..h_out {
            let mut acc = bias;
            for icg in 0..in_per_g {
                let ic = g * in_per_g + icg;
                let wbase = (oc * in_per_g + icg) * layer.kh * layer.kw;
                for ky in 0..layer.kh {
                    let iy = oy * layer.sh + ky;
                    if iy < layer.ph {
                        continue;
                    }
                    let iy = iy - layer.ph;
                    if iy >= h {
                        continue;
                    }
                    for (kx, col) in cols.iter().enumerate() {
                        let Some(col) = col else {
                            continue;
                        };
                        let wv = layer
                            .weight
                            .get(wbase + ky * layer.kw + kx)
                            .copied()
                            .unwrap_or(0.0);
                        acc = col
                            .get(ic * h + iy)
                            .copied()
                            .unwrap_or(0.0)
                            .mul_add(wv, acc);
                    }
                }
            }
            if let Some(slot) = out.get_mut(oc * h_out + oy) {
                *slot = acc;
            }
        }
    }
}

/// One GRU update: `hidden` advances by one step on input `x`.
/// `gates_in` / `gates_hid` are `3 * GRU_H` scratch, fully rewritten.
fn gru_step(
    dir: &GruDir,
    x: &[f32],
    hidden: &mut [f32],
    gates_in: &mut [f32],
    gates_hid: &mut [f32],
) {
    matvec(&dir.w_ih, &dir.b_ih, x, 3 * GRU_H, GRU_IN, gates_in);
    matvec(&dir.w_hh, &dir.b_hh, hidden, 3 * GRU_H, GRU_H, gates_hid);
    for j in 0..GRU_H {
        let reset = sigmoid(
            gates_in.get(j).copied().unwrap_or(0.0) + gates_hid.get(j).copied().unwrap_or(0.0),
        );
        let update = sigmoid(
            gates_in.get(GRU_H + j).copied().unwrap_or(0.0)
                + gates_hid.get(GRU_H + j).copied().unwrap_or(0.0),
        );
        let candidate = (gates_in.get(2 * GRU_H + j).copied().unwrap_or(0.0)
            + reset * gates_hid.get(2 * GRU_H + j).copied().unwrap_or(0.0))
        .tanh();
        let prev = hidden.get(j).copied().unwrap_or(0.0);
        let next = (1.0 - update).mul_add(candidate, update * prev);
        if let Some(slot) = hidden.get_mut(j) {
            *slot = next;
        }
    }
}

/// The forward GRU over the whole sequence; writes each step's hidden
/// state into `out` at stride [`GRU_H`].
fn gru_pass(dir: &GruDir, seq: &[f32], frames: usize, out: &mut [f32]) {
    let mut hidden = vec![0.0_f32; GRU_H];
    let mut gates_in = vec![0.0_f32; 3 * GRU_H];
    let mut gates_hid = vec![0.0_f32; 3 * GRU_H];
    for step in 0..frames {
        let x_step = seq.get(step * GRU_IN..(step + 1) * GRU_IN).unwrap_or(&[]);
        gru_step(dir, x_step, &mut hidden, &mut gates_in, &mut gates_hid);
        if let Some(dst) = out.get_mut(step * GRU_H..(step + 1) * GRU_H) {
            dst.copy_from_slice(&hidden);
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "geometry constructor mirroring the framework layer signature; \
              called five times with literal shapes from the exporter layout"
)]
fn read_conv(
    blob: &[u8],
    offset: &mut usize,
    out_c: usize,
    in_c: usize,
    kh: usize,
    kw: usize,
    sh: usize,
    sw: usize,
    ph: usize,
    pw: usize,
    groups: usize,
) -> Option<Conv> {
    let weight = read_f32s(blob, offset, out_c * (in_c / groups) * kh * kw)?;
    let bias = read_f32s(blob, offset, out_c)?;
    Some(Conv {
        weight,
        bias,
        out_c,
        in_c,
        kh,
        kw,
        sh,
        sw,
        ph,
        pw,
        groups,
    })
}

fn read_gru_dir(blob: &[u8], offset: &mut usize) -> Option<GruDir> {
    Some(GruDir {
        w_ih: read_f32s(blob, offset, 3 * GRU_H * GRU_IN)?,
        w_hh: read_f32s(blob, offset, 3 * GRU_H * GRU_H)?,
        b_ih: read_f32s(blob, offset, 3 * GRU_H)?,
        b_hh: read_f32s(blob, offset, 3 * GRU_H)?,
    })
}

/// Decoder-domain `i16` → unit-scale `f32`, matching the batch path.
fn sample_to_f32(s: i16) -> f32 {
    f32::from(s) / 32_768.0
}

/// Unit-scale `f32` → decoder-domain `i16`, matching the batch path.
fn sample_to_i16(v: f32) -> i16 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to i16 range before the cast"
    )]
    let s = (v * 32_768.0).clamp(-32_767.0, 32_767.0) as i16;
    s
}

impl Model {
    /// Parse the embedded weights.
    fn parse() -> Result<Self, LiveWaveEnhanceError> {
        let err = || LiveWaveEnhanceError::BadBlob(MODEL_BIN.len());
        let mut offset = 0usize;
        let o = &mut offset;
        let inp = read_conv(MODEL_BIN, o, CH, 4, 5, 1, 1, 1, 2, 0, 1).ok_or_else(err)?;
        let freq1 = read_conv(MODEL_BIN, o, CH, CH, 5, 3, 1, 1, 2, 1, GROUPS).ok_or_else(err)?;
        let freq2 = read_conv(MODEL_BIN, o, CH, CH, 5, 3, 1, 1, 2, 1, GROUPS).ok_or_else(err)?;
        let down = read_conv(MODEL_BIN, o, DCH, CH, 4, 1, 4, 1, 0, 0, 1).ok_or_else(err)?;
        let gru = read_gru_dir(MODEL_BIN, o).ok_or_else(err)?;
        let up_w = read_f32s(MODEL_BIN, o, GRU_IN * GRU_H).ok_or_else(err)?;
        let up_b = read_f32s(MODEL_BIN, o, GRU_IN).ok_or_else(err)?;
        let mix = read_conv(MODEL_BIN, o, CH, CH + DCH, 1, 1, 1, 1, 0, 0, 1).ok_or_else(err)?;
        let out = read_conv(MODEL_BIN, o, 2, CH, 3, 3, 1, 1, 1, 1, 1).ok_or_else(err)?;
        if offset != MODEL_BIN.len() {
            return Err(err());
        }
        let mut window = [0.0_f32; N_FFT];
        for (n, slot) in window.iter_mut().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "n < 256; exact in f32")]
            let phase = std::f32::consts::TAU * n as f32 / N_FFT as f32;
            *slot = 0.5_f32.mul_add(-phase.cos(), 0.5);
        }
        Ok(Self {
            inp,
            freq1,
            freq2,
            down,
            gru,
            up_w,
            up_b,
            mix,
            out,
            window,
        })
    }

    /// Float-domain batch processing (unit-scale samples).
    fn process_f32(&self, samples: &[f32]) -> Vec<f32> {
        let padded = Self::reflect_pad(samples);
        let frames = (padded.len() - N_FFT) / HOP + 1;
        let spec = self.stft(&padded, frames);
        let mask = self.network_mask(&Self::features(&spec, frames), frames);
        self.istft(&spec, &mask, frames, padded.len(), samples.len())
    }

    /// Centered-STFT preparation: reflect-pad `N_FFT / 2` on both ends.
    fn reflect_pad(samples: &[f32]) -> Vec<f32> {
        let pad = N_FFT / 2;
        let mut padded = Vec::with_capacity(samples.len() + 2 * pad);
        for i in (1..=pad).rev() {
            padded.push(samples.get(i).copied().unwrap_or(0.0));
        }
        padded.extend_from_slice(samples);
        for i in 1..=pad {
            padded.push(
                samples
                    .get(samples.len().wrapping_sub(1 + i))
                    .copied()
                    .unwrap_or(0.0),
            );
        }
        padded
    }

    /// Windowed forward STFT: `frames × BINS` complex, frame-major.
    fn stft(&self, padded: &[f32], frames: usize) -> Vec<Complex<f32>> {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N_FFT);
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
        spec
    }

    /// Network input: [logmag, cos, sin, t] channel-major planes of
    /// (`BINS` × frames). Inference is data prediction from t = 0, so
    /// the time-conditioning plane is identically zero.
    fn features(spec: &[Complex<f32>], frames: usize) -> Vec<f32> {
        let mut feat = vec![0.0_f32; 4 * BINS * frames];
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
        feat
    }

    /// The network body — convolutional trunk, forward recurrence,
    /// nearest-neighbor frequency upsample, and the mask head.
    /// Returns the two raw mask planes (`BINS` × frames each).
    fn network_mask(&self, feat: &[f32], frames: usize) -> Vec<f32> {
        // Convolutional trunk with residual grouped stages:
        //   trunk = gelu(inp(feat))
        //   trunk = gelu(freq1(trunk)) + trunk
        //   trunk = gelu(freq2(trunk)) + trunk
        //   down  = gelu(down(trunk))
        let (mut trunk, _, _) = conv2d(feat, BINS, frames, &self.inp);
        for v in &mut trunk {
            *v = gelu(*v);
        }
        let (mut f1, _, _) = conv2d(&trunk, BINS, frames, &self.freq1);
        for (a, b) in f1.iter_mut().zip(trunk.iter()) {
            *a = gelu(*a) + b;
        }
        let (mut f2, _, _) = conv2d(&f1, BINS, frames, &self.freq2);
        for (a, b) in f2.iter_mut().zip(f1.iter()) {
            *a = gelu(*a) + b;
        }
        let (mut down_out, _dh, _dw) = conv2d(&f2, BINS, frames, &self.down);
        for v in &mut down_out {
            *v = gelu(*v);
        }

        // Sequence for the recurrence: per time step, features in
        // [channel][freq] order.
        let mut seq = vec![0.0_f32; frames * GRU_IN];
        for c in 0..DCH {
            for fr in 0..DFR {
                for t in 0..frames {
                    let v = down_out
                        .get(c * DFR * frames + fr * frames + t)
                        .copied()
                        .unwrap_or(0.0);
                    if let Some(slot) = seq.get_mut(t * GRU_IN + c * DFR + fr) {
                        *slot = v;
                    }
                }
            }
        }
        let mut gru_out = vec![0.0_f32; frames * GRU_H];
        gru_pass(&self.gru, &seq, frames, &mut gru_out);

        // Per-step linear projection back to (DCH × DFR) planes.
        let mut g_planes = vec![0.0_f32; DCH * DFR * frames];
        let mut proj = vec![0.0_f32; GRU_IN];
        for t in 0..frames {
            let gseq = gru_out.get(t * GRU_H..(t + 1) * GRU_H).unwrap_or(&[]);
            matvec(&self.up_w, &self.up_b, gseq, GRU_IN, GRU_H, &mut proj);
            for c in 0..DCH {
                for fr in 0..DFR {
                    let v = proj.get(c * DFR + fr).copied().unwrap_or(0.0);
                    if let Some(slot) = g_planes.get_mut(c * DFR * frames + fr * frames + t) {
                        *slot = v;
                    }
                }
            }
        }

        // Concat [trunk(CH) ; nearest-upsampled recurrence(DCH)] over
        // BINS × frames, then 1×1 mix and the mask head.
        let mut cat = vec![0.0_f32; (CH + DCH) * BINS * frames];
        if let Some(dst) = cat.get_mut(..CH * BINS * frames) {
            dst.copy_from_slice(&f2);
        }
        for c in 0..DCH {
            for b in 0..BINS {
                let src_fr = b * DFR / BINS; // torch nearest: floor(i·in/out)
                for t in 0..frames {
                    let v = g_planes
                        .get(c * DFR * frames + src_fr * frames + t)
                        .copied()
                        .unwrap_or(0.0);
                    if let Some(slot) = cat.get_mut((CH + c) * BINS * frames + b * frames + t) {
                        *slot = v;
                    }
                }
            }
        }
        let (mut mixed, _, _) = conv2d(&cat, BINS, frames, &self.mix);
        for v in &mut mixed {
            *v = gelu(*v);
        }
        let (mask_planes, _, _) = conv2d(&mixed, BINS, frames, &self.out);
        mask_planes
    }

    /// Bounded complex mask application — `(1 + tanh(m0)) + i·tanh(m1)`
    /// — and window-normalized overlap-add inverse STFT, cropped back
    /// to the unpadded input length.
    fn istft(
        &self,
        spec: &[Complex<f32>],
        mask_planes: &[f32],
        frames: usize,
        padded_len: usize,
        out_len: usize,
    ) -> Vec<f32> {
        let mut planner = RealFftPlanner::<f32>::new();
        let ifft = planner.plan_fft_inverse(N_FFT);
        let mut out_padded = vec![0.0_f32; padded_len];
        let mut norm = vec![0.0_f32; padded_len];
        let mut time = ifft.make_output_vec();
        let mut freq_in = ifft.make_input_vec();
        let mut iscratch = ifft.make_scratch_vec();
        for f in 0..frames {
            for b in 0..BINS {
                let idx = b * frames + f;
                let m0 = mask_planes.get(idx).copied().unwrap_or(0.0);
                let m1 = mask_planes.get(BINS * frames + idx).copied().unwrap_or(0.0);
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
                    *slot += v * INV_N * w;
                }
                if let Some(slot) = norm.get_mut(f * HOP + n) {
                    *slot += w * w;
                }
            }
        }
        let pad = N_FFT / 2;
        (0..out_len)
            .map(|i| {
                let num = out_padded.get(pad + i).copied().unwrap_or(0.0);
                let den = norm.get(pad + i).copied().unwrap_or(1.0).max(1e-8);
                num / den
            })
            .collect()
    }
}

/// The live (causal) masking network — batch entry point and factory
/// for streaming sessions.
///
/// Holds the parsed weights behind an [`Arc`] so
/// [`stream`](Self::stream) sessions share them without copying.
#[derive(Debug)]
pub struct LiveWaveEnhancer {
    model: Arc<Model>,
}

impl LiveWaveEnhancer {
    /// Parse the embedded weights.
    ///
    /// # Errors
    ///
    /// [`LiveWaveEnhanceError::BadBlob`] when the embedded blob size
    /// does not match the compiled-in layer layout.
    pub fn new() -> Result<Self, LiveWaveEnhanceError> {
        Ok(Self {
            model: Arc::new(Model::parse()?),
        })
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
        let x: Vec<f32> = pcm.iter().copied().map(sample_to_f32).collect();
        let y = self.model.process_f32(&x);
        y.into_iter().map(sample_to_i16).collect()
    }

    /// Float-domain batch processing (unit-scale samples). The parity
    /// test drives this directly against the training checkpoint's
    /// output.
    #[must_use]
    pub fn process_f32(&self, samples: &[f32]) -> Vec<f32> {
        self.model.process_f32(samples)
    }

    /// Begin a streaming session sharing this enhancer's weights.
    #[must_use]
    pub fn stream(&self) -> LiveWaveStream {
        LiveWaveStream::new(Arc::clone(&self.model))
    }
}

/// Incremental (streaming) live-enhancement session.
///
/// Feed decoder output as it is produced — 20 ms frames via
/// [`push_frame`](Self::push_frame) or arbitrary-length unit-scale
/// float slices via [`push_samples_f32`](Self::push_samples_f32) —
/// and receive every output sample that has become final. The
/// concatenation of all per-push outputs plus the matching
/// [`finish`](Self::finish) / [`finish_f32`](Self::finish_f32) call
/// reproduces the batch API on the same total input: the stream runs
/// the identical computation (same centered reflect-padded STFT, same
/// per-column arithmetic order), buffering the first half-window and
/// mirroring it as the batch path's left reflect padding, carrying
/// the forward GRU's hidden state across pushes (starting from zero),
/// and emitting each output sample once its last overlapping analysis
/// window has been synthesized and overlap-added with the batch
/// path's squared-window normalization.
///
/// After `finish` the total emitted sample count equals the total
/// pushed sample count, and the session resets (recurrent state,
/// window accumulators, buffers) ready for the next transmission.
///
/// See the [module docs](self) for the latency budget.
pub struct LiveWaveStream {
    model: Arc<Model>,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    // FFT scratch, allocated once.
    fft_in: Vec<f32>,
    fft_scratch: Vec<Complex<f32>>,
    ifft_in: Vec<Complex<f32>>,
    ifft_out: Vec<f32>,
    ifft_scratch: Vec<Complex<f32>>,
    // Input staging.
    received: usize,
    staging: Vec<f32>,
    left_built: bool,
    pad_ring: VecDeque<f32>,
    pad_base: usize,
    tail_ring: VecDeque<f32>,
    // Per-layer column caches (rings of the most recent columns).
    next_col: usize,
    spec_cols: [Vec<Complex<f32>>; 4],
    feat_col: Vec<f32>,
    trunk_cols: [Vec<f32>; 3],
    f1_cols: [Vec<f32>; 3],
    f2_col: Vec<f32>,
    down_col: Vec<f32>,
    hidden: Vec<f32>,
    gates_in: Vec<f32>,
    gates_hid: Vec<f32>,
    proj: Vec<f32>,
    cat_col: Vec<f32>,
    mixed_cols: [Vec<f32>; 3],
    mask_col: Vec<f32>,
    // Overlap-add accumulators from the first unemitted padded
    // position onward.
    acc: VecDeque<f32>,
    norm: VecDeque<f32>,
    acc_base: usize,
    emitted: usize,
    // Output holdback and short-stream passthrough.
    pending: Vec<f32>,
    stash: Vec<i16>,
    pure_i16: bool,
}

/// Ring-buffer column lookup: `None` when `idx` exceeds `last` — the
/// batch convolutions' zero time padding at the sequence edges.
fn ring3(cols: &[Vec<f32>; 3], idx: usize, last: usize) -> Option<&[f32]> {
    if idx > last {
        return None;
    }
    cols.get(idx % 3).map(Vec::as_slice)
}

impl LiveWaveStream {
    fn new(model: Arc<Model>) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N_FFT);
        let ifft = planner.plan_fft_inverse(N_FFT);
        let fft_in = fft.make_input_vec();
        let fft_scratch = fft.make_scratch_vec();
        let ifft_in = ifft.make_input_vec();
        let ifft_out = ifft.make_output_vec();
        let ifft_scratch = ifft.make_scratch_vec();
        let spec_cols = std::array::from_fn(|_| fft.make_output_vec());
        Self {
            model,
            fft,
            ifft,
            fft_in,
            fft_scratch,
            ifft_in,
            ifft_out,
            ifft_scratch,
            received: 0,
            staging: Vec::new(),
            left_built: false,
            pad_ring: VecDeque::new(),
            pad_base: 0,
            tail_ring: VecDeque::with_capacity(PAD + 2),
            next_col: 0,
            spec_cols,
            // The fourth feature plane (time conditioning) is
            // identically zero at inference; it is written once here
            // and never touched again.
            feat_col: vec![0.0; 4 * BINS],
            trunk_cols: std::array::from_fn(|_| vec![0.0; CH * BINS]),
            f1_cols: std::array::from_fn(|_| vec![0.0; CH * BINS]),
            f2_col: vec![0.0; CH * BINS],
            down_col: vec![0.0; GRU_IN],
            hidden: vec![0.0; GRU_H],
            gates_in: vec![0.0; 3 * GRU_H],
            gates_hid: vec![0.0; 3 * GRU_H],
            proj: vec![0.0; GRU_IN],
            cat_col: vec![0.0; (CH + DCH) * BINS],
            mixed_cols: std::array::from_fn(|_| vec![0.0; CH * BINS]),
            mask_col: vec![0.0; 2 * BINS],
            acc: VecDeque::new(),
            norm: VecDeque::new(),
            acc_base: 0,
            emitted: 0,
            pending: Vec::new(),
            stash: Vec::new(),
            pure_i16: true,
        }
    }

    /// Feed one 20 ms decoder frame (160 samples at 8 kHz); returns
    /// every output sample that has become final, in order.
    ///
    /// Nothing is returned until the stream has seen 512 input
    /// samples (the batch short-clip passthrough threshold); from
    /// then on samples flow with the per-sample lookahead described
    /// in the [module docs](self).
    #[must_use]
    pub fn push_frame(&mut self, frame: &[i16; 160]) -> Vec<i16> {
        if self.pure_i16 && self.received < RELEASE_MIN {
            self.stash.extend_from_slice(frame);
        }
        for &s in frame {
            self.ingest(sample_to_f32(s));
        }
        self.pump();
        self.drain_released()
            .into_iter()
            .map(sample_to_i16)
            .collect()
    }

    /// Feed unit-scale float samples (any length, including empty);
    /// returns every output sample that has become final, in order.
    ///
    /// Float-domain twin of [`push_frame`](Self::push_frame): the
    /// concatenation of all per-push outputs plus
    /// [`finish_f32`](Self::finish_f32) equals
    /// [`LiveWaveEnhancer::process_f32`] of the concatenated input.
    #[must_use]
    pub fn push_samples_f32(&mut self, samples: &[f32]) -> Vec<f32> {
        if self.pure_i16 {
            self.pure_i16 = false;
            self.stash.clear();
        }
        for &s in samples {
            self.ingest(s);
        }
        self.pump();
        self.drain_released()
    }

    /// Flush the tail and reset the session for reuse.
    ///
    /// Mirrors the batch path's right reflect padding, runs the
    /// remaining spectral columns (with the batch convolutions' zero
    /// time padding at the sequence edge), and drains every unemitted
    /// sample, so the total streamed output length equals the total
    /// pushed input length.
    ///
    /// A session fed exclusively through
    /// [`push_frame`](Self::push_frame) that ends before 512 total
    /// samples returns the input verbatim, matching
    /// [`LiveWaveEnhancer::process`]'s short-clip passthrough.
    /// Otherwise the enhanced tail is returned.
    #[must_use]
    pub fn finish(&mut self) -> Vec<i16> {
        if self.pure_i16 && self.received < RELEASE_MIN {
            let out = std::mem::take(&mut self.stash);
            self.reset();
            return out;
        }
        self.finish_f32().into_iter().map(sample_to_i16).collect()
    }

    /// Float-domain [`finish`](Self::finish): always runs the
    /// enhancement pipeline — like
    /// [`LiveWaveEnhancer::process_f32`], which has no short-clip
    /// passthrough — then flushes and resets the session.
    #[must_use]
    pub fn finish_f32(&mut self) -> Vec<f32> {
        let len = self.received;
        if len == 0 {
            self.reset();
            return Vec::new();
        }
        if !self.left_built {
            self.build_left();
        }
        self.append_right_pad();
        let total = len / HOP + 1;
        while self.next_col < total {
            let k = self.next_col;
            self.stft_col(k);
            self.cascade(k, total - 1);
            self.next_col += 1;
            self.trim_pad();
        }
        for k in total..total + LOOKAHEAD_COLS {
            self.cascade(k, total - 1);
        }
        self.emit_ready(PAD + len - 1);
        let out = std::mem::take(&mut self.pending);
        self.reset();
        out
    }

    /// Accept one input sample into the staging/padded buffers.
    fn ingest(&mut self, v: f32) {
        if self.left_built {
            self.pad_ring.push_back(v);
        } else {
            self.staging.push(v);
        }
        self.tail_ring.push_back(v);
        if self.tail_ring.len() > PAD + 1 {
            let _ = self.tail_ring.pop_front();
        }
        self.received += 1;
    }

    /// Build the left reflect padding (`padded[j] = samples[PAD - j]`,
    /// zero-filled where the input is shorter, exactly as the batch
    /// path's `reflect_pad`) and move the staged samples after it.
    fn build_left(&mut self) {
        for j in 0..PAD {
            let v = self.staging.get(PAD - j).copied().unwrap_or(0.0);
            self.pad_ring.push_back(v);
        }
        self.pad_ring.extend(self.staging.drain(..));
        self.left_built = true;
    }

    /// Append the right reflect padding (`samples[len - 1 - i]` for
    /// `i` in `1..=PAD`, zero-filled where the input is shorter) from
    /// the retained tail samples.
    fn append_right_pad(&mut self) {
        let tlen = self.tail_ring.len();
        for i in 1..=PAD {
            let v = tlen
                .checked_sub(1 + i)
                .and_then(|rel| self.tail_ring.get(rel))
                .copied()
                .unwrap_or(0.0);
            self.pad_ring.push_back(v);
        }
    }

    /// Process every STFT column whose full analysis window has
    /// arrived.
    fn pump(&mut self) {
        if !self.left_built && self.received > PAD {
            self.build_left();
        }
        if !self.left_built {
            return;
        }
        while HOP * self.next_col + N_FFT <= PAD + self.received {
            let k = self.next_col;
            self.stft_col(k);
            self.cascade(k, k);
            self.next_col += 1;
            self.trim_pad();
        }
    }

    /// Windowed forward transform of column `k` into its ring slot.
    fn stft_col(&mut self, k: usize) {
        let start = (HOP * k).saturating_sub(self.pad_base);
        for (n, slot) in self.fft_in.iter_mut().enumerate() {
            *slot = self.pad_ring.get(start + n).copied().unwrap_or(0.0)
                * self.model.window.get(n).copied().unwrap_or(0.0);
        }
        if let Some(spec) = self.spec_cols.get_mut(k % 4) {
            let _ = self
                .fft
                .process_with_scratch(&mut self.fft_in, spec, &mut self.fft_scratch);
        }
    }

    /// Drop padded samples no future analysis window can reach.
    fn trim_pad(&mut self) {
        let keep_from = HOP * self.next_col;
        while self.pad_base < keep_from {
            if self.pad_ring.pop_front().is_none() {
                break;
            }
            self.pad_base += 1;
        }
    }

    /// Advance every layer that gains a new column when STFT column
    /// `k` lands. `last` is the highest column index that exists;
    /// columns beyond it are the batch convolutions' zero padding
    /// (`k > last` itself occurs only while draining the tail).
    fn cascade(&mut self, k: usize, last: usize) {
        self.step_trunk(k, last);
        self.step_f1(k, last);
        self.step_recurrent(k, last);
        self.step_mask(k, last);
    }

    /// Features + `inp` (time-pointwise) + GELU → trunk column `k`.
    fn step_trunk(&mut self, k: usize, last: usize) {
        if k > last {
            return;
        }
        let Some(spec) = self.spec_cols.get(k % 4) else {
            return;
        };
        for b in 0..BINS {
            let c = spec.get(b).copied().unwrap_or_default();
            let mag = c.norm().max(1e-6);
            if let Some(s) = self.feat_col.get_mut(b) {
                *s = mag.ln();
            }
            if let Some(s) = self.feat_col.get_mut(BINS + b) {
                *s = c.re / mag;
            }
            if let Some(s) = self.feat_col.get_mut(2 * BINS + b) {
                *s = c.im / mag;
            }
        }
        let Some(slot) = self.trunk_cols.get_mut(k % 3) else {
            return;
        };
        conv_col(
            &self.model.inp,
            &[Some(self.feat_col.as_slice())],
            BINS,
            slot,
        );
        for v in slot.iter_mut() {
            *v = gelu(*v);
        }
    }

    /// `freq1` (one column behind `k`) + GELU + residual.
    fn step_f1(&mut self, k: usize, last: usize) {
        let Some(j) = k.checked_sub(1) else {
            return;
        };
        if j > last {
            return;
        }
        let cols = [
            j.checked_sub(1)
                .and_then(|i| ring3(&self.trunk_cols, i, last)),
            ring3(&self.trunk_cols, j, last),
            ring3(&self.trunk_cols, j + 1, last),
        ];
        let Some(slot) = self.f1_cols.get_mut(j % 3) else {
            return;
        };
        conv_col(&self.model.freq1, &cols, BINS, slot);
        let Some(trunk) = self.trunk_cols.get(j % 3) else {
            return;
        };
        for (a, b) in slot.iter_mut().zip(trunk.iter()) {
            *a = gelu(*a) + b;
        }
    }

    /// `freq2` + residual, `down`, one GRU step, `up` projection,
    /// concat with nearest-neighbor frequency upsample, and `mix`
    /// (all two columns behind `k`).
    fn step_recurrent(&mut self, k: usize, last: usize) {
        let Some(j) = k.checked_sub(2) else {
            return;
        };
        if j > last {
            return;
        }
        {
            let cols = [
                j.checked_sub(1).and_then(|i| ring3(&self.f1_cols, i, last)),
                ring3(&self.f1_cols, j, last),
                ring3(&self.f1_cols, j + 1, last),
            ];
            conv_col(&self.model.freq2, &cols, BINS, &mut self.f2_col);
        }
        if let Some(res) = self.f1_cols.get(j % 3) {
            for (a, b) in self.f2_col.iter_mut().zip(res.iter()) {
                *a = gelu(*a) + b;
            }
        }
        conv_col(
            &self.model.down,
            &[Some(self.f2_col.as_slice())],
            BINS,
            &mut self.down_col,
        );
        for v in &mut self.down_col {
            *v = gelu(*v);
        }
        gru_step(
            &self.model.gru,
            &self.down_col,
            &mut self.hidden,
            &mut self.gates_in,
            &mut self.gates_hid,
        );
        matvec(
            &self.model.up_w,
            &self.model.up_b,
            &self.hidden,
            GRU_IN,
            GRU_H,
            &mut self.proj,
        );
        if let Some(dst) = self.cat_col.get_mut(..CH * BINS) {
            dst.copy_from_slice(&self.f2_col);
        }
        for c in 0..DCH {
            for b in 0..BINS {
                // torch nearest: floor(i·in/out)
                let v = self
                    .proj
                    .get(c * DFR + b * DFR / BINS)
                    .copied()
                    .unwrap_or(0.0);
                if let Some(slot) = self.cat_col.get_mut((CH + c) * BINS + b) {
                    *slot = v;
                }
            }
        }
        let Some(slot) = self.mixed_cols.get_mut(j % 3) else {
            return;
        };
        conv_col(
            &self.model.mix,
            &[Some(self.cat_col.as_slice())],
            BINS,
            slot,
        );
        for v in slot.iter_mut() {
            *v = gelu(*v);
        }
    }

    /// Mask head (three columns behind `k`), bounded complex mask
    /// application, inverse transform, overlap-add, and emission of
    /// every sample whose last overlapping window just landed.
    fn step_mask(&mut self, k: usize, last: usize) {
        let Some(m) = k.checked_sub(LOOKAHEAD_COLS) else {
            return;
        };
        if m > last {
            return;
        }
        {
            let cols = [
                m.checked_sub(1)
                    .and_then(|i| ring3(&self.mixed_cols, i, last)),
                ring3(&self.mixed_cols, m, last),
                ring3(&self.mixed_cols, m + 1, last),
            ];
            conv_col(&self.model.out, &cols, BINS, &mut self.mask_col);
        }
        let Some(spec) = self.spec_cols.get(m % 4) else {
            return;
        };
        for b in 0..BINS {
            let m0 = self.mask_col.get(b).copied().unwrap_or(0.0);
            let m1 = self.mask_col.get(BINS + b).copied().unwrap_or(0.0);
            let mask = Complex::new(1.0 + m0.tanh(), m1.tanh());
            let c = spec.get(b).copied().unwrap_or_default();
            if let Some(slot) = self.ifft_in.get_mut(b) {
                *slot = c * mask;
            }
        }
        let _ = self.ifft.process_with_scratch(
            &mut self.ifft_in,
            &mut self.ifft_out,
            &mut self.ifft_scratch,
        );
        let start = HOP * m;
        while self.acc_base + self.acc.len() < start + N_FFT {
            self.acc.push_back(0.0);
            self.norm.push_back(0.0);
        }
        for (n, &v) in self.ifft_out.iter().enumerate() {
            let w = self.model.window.get(n).copied().unwrap_or(0.0);
            let Some(rel) = (start + n).checked_sub(self.acc_base) else {
                continue;
            };
            if let Some(slot) = self.acc.get_mut(rel) {
                *slot += v * INV_N * w;
            }
            if let Some(slot) = self.norm.get_mut(rel) {
                *slot += w * w;
            }
        }
        self.emit_ready(start + HOP - 1);
    }

    /// Move every final sample — padded position at most `through`,
    /// i.e. all of whose overlapping windows have been accumulated —
    /// into the pending output, dividing by the accumulated
    /// squared-window norm exactly as the batch path does.
    fn emit_ready(&mut self, through: usize) {
        if PAD + self.emitted > through {
            return;
        }
        // Discard the left padding region (positions before PAD are
        // synthesized but cropped, exactly as in the batch path).
        while self.acc_base < PAD + self.emitted {
            let _ = self.acc.pop_front();
            let _ = self.norm.pop_front();
            self.acc_base += 1;
        }
        while PAD + self.emitted <= through {
            let num = self.acc.pop_front().unwrap_or(0.0);
            let den = self.norm.pop_front().unwrap_or(1.0).max(1e-8);
            self.pending.push(num / den);
            self.acc_base += 1;
            self.emitted += 1;
        }
    }

    /// Hand pending samples to the caller once the release threshold
    /// has been met.
    fn drain_released(&mut self) -> Vec<f32> {
        if self.received < RELEASE_MIN {
            return Vec::new();
        }
        self.stash.clear();
        std::mem::take(&mut self.pending)
    }

    /// Return to the pristine post-construction state (weights and
    /// scratch allocations are kept).
    fn reset(&mut self) {
        self.received = 0;
        self.staging.clear();
        self.left_built = false;
        self.pad_ring.clear();
        self.pad_base = 0;
        self.tail_ring.clear();
        self.next_col = 0;
        for v in &mut self.hidden {
            *v = 0.0;
        }
        self.acc.clear();
        self.norm.clear();
        self.acc_base = 0;
        self.emitted = 0;
        self.pending.clear();
        self.stash.clear();
        self.pure_i16 = true;
    }
}

impl std::fmt::Debug for LiveWaveStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveWaveStream")
            .field("received", &self.received)
            .field("emitted", &self.emitted)
            .field("next_col", &self.next_col)
            .field("pending_samples", &self.pending.len())
            .finish_non_exhaustive()
    }
}
