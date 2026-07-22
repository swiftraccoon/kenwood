// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Learned waveform enhancement (complex-STFT masking).
//!
//! Blind listening located the audible distance between this decoder
//! and reference hardware-grade decodes of identical AMBE frames in
//! waveform fine structure, and the shipped model here is the first
//! checkpoint to clear the operator listening bar (unanimous
//! preference across a speaker-diverse blind review): a small
//! grouped-convolution network with a bidirectional recurrent core,
//! fine-tuned adversarially against reference decodes, predicting a
//! bounded complex mask (magnitude *and* phase corrections around
//! identity) over the decoder output's STFT. Offline/whole-clip
//! processing; the recurrence reads the entire clip.
//!
//! The forward pass reproduces the training framework's semantics:
//! centered reflect-padded STFT (256/64, Hann), exact-erf GELU,
//! grouped and strided convolutions, a bidirectional GRU, nearest-
//! neighbor frequency upsampling, and window-normalized inverse STFT.
//! It is pinned against a recorded reference vector produced by
//! the training checkpoint.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

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
/// Recurrent hidden width per direction.
const GRU_H: usize = 128;

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

/// One GRU direction's parameters (gate order: reset, update, new).
#[derive(Debug)]
struct GruDir {
    w_ih: Vec<f32>, // [3*GRU_H][GRU_IN]
    w_hh: Vec<f32>, // [3*GRU_H][GRU_H]
    b_ih: Vec<f32>,
    b_hh: Vec<f32>,
}

/// The full masking network.
#[derive(Debug)]
pub struct WaveEnhancer {
    inp: Conv,
    freq1: Conv,
    freq2: Conv,
    down: Conv,
    gru_f: GruDir,
    gru_r: GruDir,
    up_w: Vec<f32>, // [GRU_IN][2*GRU_H]
    up_b: Vec<f32>,
    mix: Conv,
    out: Conv,
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

/// One GRU direction over the whole sequence; writes each step's
/// hidden state into `out` at stride `2 * GRU_H` starting at `phase`.
fn gru_pass(
    dir: &GruDir,
    seq: &[f32],
    frames: usize,
    reverse: bool,
    out: &mut [f32],
    phase: usize,
) {
    let mut hidden = vec![0.0_f32; GRU_H];
    let mut gates_in = vec![0.0_f32; 3 * GRU_H];
    let mut gates_hid = vec![0.0_f32; 3 * GRU_H];
    for step in 0..frames {
        let tick = if reverse { frames - 1 - step } else { step };
        let x_step = seq.get(tick * GRU_IN..(tick + 1) * GRU_IN).unwrap_or(&[]);
        matvec(
            &dir.w_ih,
            &dir.b_ih,
            x_step,
            3 * GRU_H,
            GRU_IN,
            &mut gates_in,
        );
        matvec(
            &dir.w_hh,
            &dir.b_hh,
            &hidden,
            3 * GRU_H,
            GRU_H,
            &mut gates_hid,
        );
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
            if let Some(slot) = out.get_mut(tick * 2 * GRU_H + phase + j) {
                *slot = next;
            }
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

impl WaveEnhancer {
    /// Parse the embedded weights.
    ///
    /// # Errors
    ///
    /// [`WaveEnhanceError::BadBlob`] when the embedded blob size does
    /// not match the compiled-in layer layout.
    pub fn new() -> Result<Self, WaveEnhanceError> {
        let err = || WaveEnhanceError::BadBlob(MODEL_BIN.len());
        let mut offset = 0usize;
        let o = &mut offset;
        let inp = read_conv(MODEL_BIN, o, CH, 4, 5, 1, 1, 1, 2, 0, 1).ok_or_else(err)?;
        let freq1 = read_conv(MODEL_BIN, o, CH, CH, 5, 3, 1, 1, 2, 1, GROUPS).ok_or_else(err)?;
        let freq2 = read_conv(MODEL_BIN, o, CH, CH, 5, 3, 1, 1, 2, 1, GROUPS).ok_or_else(err)?;
        let down = read_conv(MODEL_BIN, o, DCH, CH, 4, 1, 4, 1, 0, 0, 1).ok_or_else(err)?;
        let gru_f = read_gru_dir(MODEL_BIN, o).ok_or_else(err)?;
        let gru_r = read_gru_dir(MODEL_BIN, o).ok_or_else(err)?;
        let up_w = read_f32s(MODEL_BIN, o, GRU_IN * 2 * GRU_H).ok_or_else(err)?;
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
            gru_f,
            gru_r,
            up_w,
            up_b,
            mix,
            out,
            window,
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
    pub fn process_f32(&self, samples: &[f32]) -> Vec<f32> {
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

    /// The network body: convolutional trunk, bidirectional
    /// recurrence, nearest-neighbor frequency upsample, and the mask
    /// head. Returns the two raw mask planes (`BINS` × frames each).
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
        let mut gru_out = vec![0.0_f32; frames * 2 * GRU_H];
        gru_pass(&self.gru_f, &seq, frames, false, &mut gru_out, 0);
        gru_pass(&self.gru_r, &seq, frames, true, &mut gru_out, GRU_H);

        // Per-step linear projection back to (DCH × DFR) planes.
        let mut g_planes = vec![0.0_f32; DCH * DFR * frames];
        let mut proj = vec![0.0_f32; GRU_IN];
        for t in 0..frames {
            let gseq = gru_out
                .get(t * 2 * GRU_H..(t + 1) * 2 * GRU_H)
                .unwrap_or(&[]);
            matvec(&self.up_w, &self.up_b, gseq, GRU_IN, 2 * GRU_H, &mut proj);
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

    /// Bounded complex mask application, `(1 + tanh(m0)) + i·tanh(m1)`,
    /// followed by window-normalized overlap-add inverse STFT, cropped
    /// back to the unpadded input length.
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
        #[expect(clippy::cast_precision_loss, reason = "N_FFT is 256; exact in f32")]
        let inv_n = 1.0 / N_FFT as f32;
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
                    *slot += v * inv_n * w;
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
