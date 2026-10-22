# mbelib-rs

AMBE 3600×2400 codec and PCM enhancement for D-STAR. One nine-byte wire
frame produces 160 signed 16-bit PCM samples at 8 kHz, or 20 ms of audio.
Each decoder and encoder owns one stream's prediction and synthesis history.
The crate contains no handwritten unsafe code or FFI.

The decoder is always available. The `encoder` feature adds an experimental
encoder and checked diagnostic stages. Hardware interoperability of the encoder
remains unverified; the ignored spectral-envelope correlation regression is an
existing qualification requirement. AMBE+, AMBE+2 and IMBE are outside the
implemented wire format.

## Decode and inspect a stream

Arbitrary nine-byte frames are valid decoder inputs. `frame_fec` reports the
correction counts and frame classification; `decode_trace` exposes immutable
codebook indices, radians per sample, harmonic count and corrected bits.
`AmbeParamExtractor` performs stateful parameter extraction without synthesis.

```rust
use mbelib_rs::{AmbeDecoder, AmbeParamExtractor, decode_trace};

let frame = [0x9e, 0x8d, 0x32, 0x88, 0x26, 0x1a, 0x3f, 0x61, 0xe8];
let mut decoder = AmbeDecoder::new();
let pcm: [i16; 160] = decoder.decode_frame(&frame);
assert_eq!(pcm.len(), 160);
let trace = decode_trace(&frame);
assert!(trace.data_bits().iter().all(|&bit| bit <= 1));
let mut extractor = AmbeParamExtractor::new();
let parameters = extractor.extract(&frame);
assert!(parameters.harmonics() <= 56);
let concealed: [i16; 160] = decoder.conceal_frame();
assert_eq!(concealed.len(), 160);
```

Erasure and invalid-tone frames emit silence and clear stream history. Sustained
FEC failures repeat prior parameters before muting. Tone frames use the tone
oscillator. History resets restore PARITY coefficients while retaining the
selected envelope algorithm. A fresh decoder starts with deterministic reference
tuning and zero stream history.

## Checked PCM

`PcmFrame` contains exactly 160 finite `f32` samples in inclusive `[-1, 1]`.
`PcmSamples` validates any borrowed block in the same amplitude domain, including
an empty block. Constructors reject invalid lengths, NaN, infinity and samples
outside the domain. Signed 16-bit frame conversion divides by 32768.

```rust
use mbelib_rs::{PcmFrame, PcmSamples};

let frame = PcmFrame::new([0.0; 160])?;
assert_eq!(frame.as_pcm_samples().as_slice().len(), 160);
assert!(PcmSamples::new(&[f32::NAN]).is_err());
let normalized = PcmFrame::from([i16::MIN; 160]);
assert_eq!(normalized.samples()[0], -1.0);
# Ok::<(), Box<dyn std::error::Error>>(())
```

```rust
# #[cfg(feature = "encoder")]
# {
use mbelib_rs::{AmbeEncoder, PcmFrame};

let mut encoder = AmbeEncoder::new();
let frame = PcmFrame::new([0.0; 160])?;
let wire: [u8; 9] = encoder.encode_frame(&frame);
let from_integer: [u8; 9] = encoder.encode_frame_i16(&[0; 160]);
assert_eq!(wire.len(), from_integer.len());
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

Encoder construction allocates FFT plans, reusable scratch and optional lookahead
slots. Frame processing reuses this storage. `EncoderOptions` selects the input
filter and single-frame or two-frame-lookahead pitch tracking; lookahead adds two
20 ms frames and emits silence while filling. It has no implicit tail flush.

## Inspect the encoder stages

Diagnostic stages are public checked contracts. `Spectrum` has 129 complex bins
from a 256-point real FFT. `PitchHistory` borrows 301 finite samples. Pitch
periods are measured in samples, frequencies in Hz or FFT bins as named, and
`PitchMetric` is in `[0, 1]`. Related count/array constructors reject excessive
counts or nonzero padding. Public numerical stages return `StageError` if a
finite candidate causes an unrepresentable intermediate result; failed stateful
estimation leaves its history unchanged.

```rust
# #[cfg(feature = "encoder")]
# {
use mbelib_rs::{EncoderBuffers, FftPlan, FundamentalBin, PcmFrame,
    PitchMetric, PitchTracker, VuvState, analyze_frame, detect_vuv_and_sa};
use mbelib_rs::validation::{PrevFrameState, quantize};

let mut buffers = EncoderBuffers::new();
let mut plan = FftPlan::new();
let spectrum = analyze_frame(&PcmFrame::new([0.0; 160])?, &mut buffers, &mut plan);
let pitch = PitchTracker::new().estimate(buffers.pitch_est_buf())?;
let bin = FundamentalBin::new(256.0 / pitch.period_samples())?;
let (voicing, amplitudes) = detect_vuv_and_sa(&spectrum, bin,
    &mut VuvState::default(), PitchMetric::new(1.0 - pitch.confidence())?)?;
let outcome = quantize(pitch, voicing, &amplitudes, &PrevFrameState::ZERO)?;
assert!(outcome.data_bits().iter().all(|&bit| bit <= 1));
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Tuning and optional algorithms

`SynthesisTuning::new(TuningSpec)` checks finite, nonnegative coefficients and
ordered enhancement bounds. `PARITY` is the default. Large coefficients can
saturate signed PCM output; construction establishes the parameter domain,
not a perceptual quality target.

```rust
use mbelib_rs::{AmbeDecoder, SynthesisTuning, TuningSpec};

let tuning = SynthesisTuning::new(TuningSpec {
    phase_jitter: 0.0,
    ..SynthesisTuning::PARITY.spec()
})?;
let decoder = AmbeDecoder::with_tuning(tuning);
assert_eq!(decoder.tuning(), tuning);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`kenwood-tables` exposes coefficient data and explicit `InputFilter::Kenwood`
and `EnvelopePostfilter::Kenwood` choices. Enabling the feature leaves default
encoder and decoder algorithms at their reference settings. Changing tuning,
filter choice or waveform models changes the audio configuration; synthesis or
enhancement changes require the crate's blind listening qualification.

## Waveform enhancement

`wave-enhance` adds offline and causal waveform models with embedded immutable
weights. Construction parses the model and allocates its caches. Whole-clip
processing allocates in proportion to the clip. The causal stream retains
bounded staging, model history and output tail regardless of input block size;
each push allocates the returned output in proportion to the ready samples.
Call `finish` to drain the tail and reset a stream for reuse. Dropping without
finishing discards delayed samples. Total output after finishing equals input
length; no clock or I/O is read by these processors.

```rust
# #[cfg(feature = "wave-enhance")]
# {
use mbelib_rs::enhance_live::LiveWaveEnhancer;
use mbelib_rs::enhance_wave::WaveEnhancer;

let decoded = [0_i16; 160];
let offline = WaveEnhancer::new()?;
assert_eq!(offline.process(&decoded).len(), decoded.len());
let live = LiveWaveEnhancer::new()?;
let mut stream = live.stream();
let mut output = stream.push_frame(&decoded);
output.extend(stream.finish());
assert_eq!(output.len(), decoded.len());
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Stream tools

```sh
cargo run -p mbelib-rs --example decode_ambe_stream -- input.ambe output.s16
cargo run -p mbelib-rs --features encoder --example encode_ambe_stream -- input.s16 output.ambe
```

The `validate_analysis_vs_op25`, `validate_bvec_vs_op25`,
`validate_dp_vs_op25` and `validate_quantize_vs_op25` examples compare supplied
PCM or parameter traces with OP25. Their outputs describe the supplied fixture;
they perform no hardware qualification. Historical TH-D75 anchor fixtures require
recapture with verified framing before they can qualify encoder output.

## Dependencies and license

`realfft` supplies real FFT plans and `wide` supplies SIMD conversion primitives.
The decoder uses GPL-2.0-or-later licensing; the optional OP25-derived encoder
contains GPL-3.0-or-later code. The upstream attribution and applicable texts are
in [LICENSE](https://github.com/swiftraccoon/kenwood/blob/main/mbelib-rs/LICENSE)
and [LICENSES](https://github.com/swiftraccoon/kenwood/tree/main/mbelib-rs/LICENSES).
