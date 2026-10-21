# if-dsp

Sans-io processing for a mono 48 kHz audio stream containing a 12 kHz low IF.
The channelizer mixes that IF to complex baseband, decimates to 12 kHz,
filters and demodulates USB, LSB, CW or AM, applies optional AGC, and
interpolates audio back to 48 kHz. Device capture, tuning, playback and clocks
belong to the caller.

## Configure and process an IF stream

`ChannelizerConfig` has validated private fields. Widths are finite and positive;
the mode's passband stays strictly inside the 6 kHz baseband Nyquist edges.
USB and LSB widths are below 5,900 Hz, CW widths below 10,600 Hz around its
700 Hz pitch, and AM cutoffs below 6,000 Hz. Widths too small to produce distinct
SSB or CW edges at floating-point precision are rejected too.

```rust
use if_dsp::{Bandwidth, Channelizer, ChannelizerConfig, DemodMode};

let config = ChannelizerConfig::new(DemodMode::Usb)
    .with_filter_width(Bandwidth::new(2_600.0)?)?;
let mut receiver = Channelizer::new(config)?;
let mut audio = Vec::with_capacity(480);
receiver.process(&[0.0; 480], &mut audio);
assert_eq!(audio.len(), 480);
receiver.set_mode(DemodMode::Lsb)?;
receiver.set_filter_width(Some(Bandwidth::new(1_800.0)?))?;
let previous = receiver.filter_hz();
assert!(receiver.set_filter_width(Some(Bandwidth::new(6_000.0)?)).is_err());
assert_eq!(receiver.filter_hz(), previous);
receiver.set_filter_width(None)?;
assert_eq!(receiver.filter_hz(), DemodMode::Lsb.default_filter_hz());
# Ok::<(), if_dsp::DspError>(())
```

The first sample starts the decimator's four-sample cycle. Each emitted
baseband sample produces four output samples, so a single input sample can
produce four outputs while later short blocks produce none. Concatenated
chunk outputs equal the output for concatenated input. Empty blocks clear
output and retain state.

Mode changes select the mode's default width. Width and mode changes reset
the mode filter's history and keep the mixer, rate converters and AGC state.
The new filter has its normal startup transient. A rejected change preserves
every previous stage and configuration value.

## Use standalone stages

`Nco` wraps its phase and reduces finite frequencies modulo the positive sample
rate before computing a step. `AgcConfig` checks each positive finite parameter;
`Agc::new` also requires a positive tracking coefficient at `f32` precision.

```rust
use if_dsp::{Agc, AgcConfig, Frequency, Nco, SampleRate};

let rate = SampleRate::new(12_000.0)?;
let mut oscillator = Nco::new(Frequency::new(-1_000.0)?, rate);
assert!((oscillator.next_sample().norm() - 1.0).abs() < 1e-6);
let config = AgcConfig::new().with_target(0.25)?.with_attack_ms(5.0)?
    .with_decay_ms(250.0)?.with_max_gain(1_000.0)?;
let mut agc = Agc::new(config, rate)?;
let mut samples = [0.01; 120];
agc.process(&mut samples);
assert!(samples.iter().all(|sample| sample.is_finite()));
# Ok::<(), if_dsp::DspError>(())
```

SSB takes twice the real part after a complex passband, restoring the amplitude
halved by a real IF mix. CW uses the same method around a chosen pitch.
AM filters complex baseband, takes its magnitude, and applies a single-pole
DC blocker near 50 Hz. Its amplitude follows carrier level and modulation depth.

```rust
use if_dsp::{Bandwidth, Complex32, Frequency, SampleRate, TapCount};
use if_dsp::demod::{AmDemod, CwDemod, SsbDemod};
use if_dsp::fir::{BandpassDesign, LowpassDesign};

let rate = SampleRate::new(12_000.0)?;
let taps = TapCount::new(127)?;
let band = BandpassDesign::new(Frequency::new(100.0)?..Frequency::new(2_700.0)?, rate, taps)?;
let mut ssb = SsbDemod::new(band)?;
let mut cw = CwDemod::new(Frequency::new(700.0)?, Bandwidth::new(500.0)?, rate, taps)?;
let mut am = AmDemod::new(LowpassDesign::new(Bandwidth::new(4_500.0)?, rate, taps)?)?;
let input = [Complex32::new(0.5, 0.0); 16];
let mut audio = Vec::new();
ssb.process(&input, &mut audio);
cw.process(&input, &mut audio);
am.process(&input, &mut audio);
assert_eq!(audio.len(), input.len());
# Ok::<(), if_dsp::DspError>(())
```

## Design and stream a filter

Windowed-sinc designs use odd tap counts of at least three and a Hamming window.
Cutoffs stay below Nyquist. Custom streaming FIR coefficients may have any
positive length, including a one-tap identity filter; all coefficients are finite.
Constructors retain coefficients and allocate zeroed history. Pushes allocate nothing.

```rust
use if_dsp::{Bandwidth, SampleRate, TapCount};
use if_dsp::fir::{FirReal, FirRealTaps, LowpassDesign};

let design = LowpassDesign::new(Bandwidth::new(5_000.0)?,
    SampleRate::new(48_000.0)?, TapCount::new(63)?)?;
let taps = design.to_taps();
let mut real_filter = FirReal::new(taps.clone())?;
let mut complex_input_filter = FirRealTaps::new(taps)?;
let _real = real_filter.push(1.0);
let _complex = complex_input_filter.push(if_dsp::Complex32::new(1.0, 0.0));
assert!(FirReal::new(Vec::new()).is_err());
# Ok::<(), if_dsp::DspError>(())
```

`BandpassDesign` uses signed edges to select either sideband. Edges are ordered
and strictly inside both Nyquist boundaries. Its normalized prototype cutoff
must remain positive at `f64` precision.

```rust
use if_dsp::{Complex32, Frequency, SampleRate, TapCount};
use if_dsp::fir::{BandpassDesign, FirComplexTaps};

let design = BandpassDesign::new(Frequency::new(-2_700.0)?..Frequency::new(-100.0)?,
    SampleRate::new(12_000.0)?, TapCount::new(127)?)?;
let mut filter = FirComplexTaps::new(design.to_taps())?;
let sample = filter.push(Complex32::new(1.0, 0.0));
assert!(sample.re.is_finite() && sample.im.is_finite());
# Ok::<(), if_dsp::DspError>(())
```

## Convert sample rates

Both converters require a positive integer factor. Their cutoff is strictly
below `design.sample_rate() / (2 * factor)`: the design rate is the decimator
input rate or interpolator output rate. This comparison uses `f64`; integer
factors above its exact range are rounded. Interpolation gain is the factor
rounded to `f32` precision.

```rust
use if_dsp::{Bandwidth, Complex32, RateFactor, SampleRate, TapCount};
use if_dsp::fir::LowpassDesign;
use if_dsp::resample::{Decimator, Interpolator};

let factor = RateFactor::new(4)?;
let design = LowpassDesign::new(Bandwidth::new(5_000.0)?,
    SampleRate::new(48_000.0)?, TapCount::new(63)?)?;
let mut decimator = Decimator::new(factor, design)?;
let mut interpolator = Interpolator::new(factor, design)?;
let mut baseband = Vec::new();
decimator.process(&[Complex32::new(0.0, 0.0); 48], &mut baseband);
assert_eq!(baseband.len(), 12);
let real: Vec<_> = baseband.iter().map(|sample| sample.re).collect();
let mut audio = Vec::new();
interpolator.process(&real, &mut audio);
assert_eq!(audio.len(), 48);
# Ok::<(), if_dsp::DspError>(())
```

## Measure a spectrum

`SpectrumEstimator` averages squared FFT magnitudes of periodic Hann-windowed
segments. A segment has at least two samples and advances by `size / 2` samples;
odd sizes use the same integer hop. Output has `size / 2 + 1` nonnegative bins.
There is no one-sided doubling, sample-rate division or window normalization.
The values are uncalibrated bin powers; applications apply their amplitude,
dB or power spectral density calibration without another transform.
`window_sum()` supplies the actual coherent gain. For an interior bin-centered
real tone, peak amplitude is approximately `2 * sqrt(power) / window_sum`.
DC and even-size Nyquist bins omit the factor of two.

```rust
use if_dsp::{FftSize, Frequency, SampleRate, SpectrumEstimator};
use if_dsp::spectrum::goertzel;

let rate = SampleRate::new(48_000.0)?;
let mut estimator = SpectrumEstimator::new(FftSize::new(1_024)?);
estimator.feed(&[0.0; 2_048]);
let mut powers = Vec::with_capacity(513);
estimator.write_bin_power(&mut powers);
assert_eq!(powers.len(), 513);
assert!((estimator.window_sum() - 512.0).abs() < 1e-3);
assert_eq!(estimator.peak_hz(rate), 0.0);
estimator.reset();
assert_eq!(goertzel([1.0; 16], rate, Frequency::new(0.0)?).to_bits(), 1.0_f32.to_bits());
# Ok::<(), if_dsp::DspError>(())
```

## Allocation and samples

The crate performs no I/O, reads no clock and creates no threads. Channelizer,
demodulator and rate-converter calls clear caller-owned output and retain stream
state. Their output vectors and channelizer scratch can allocate when a block
exceeds current capacity. Once sufficient, storage is reused. Reconfiguration
allocates new coefficients and filter history.

The spectrum estimator allocates its FFT plan, window, one-segment pending
buffer, transform buffer and FFT scratch at construction. `feed` retains these
capacities for arbitrary block sizes. Writing powers can grow caller-owned
output; `reset` retains allocations. Standard `Vec` capacity overflow panics;
allocation exhaustion follows the allocator's failure behavior. Numeric metadata
validation does not impose application memory or CPU budgets.

PCM buffers contain primitive `f32` values. Callers supply finite samples and
choose amplitudes whose intermediate calculations fit floating-point storage.
Stages do not clip, sanitize or scan every sample. Nonfinite input and overflow
can propagate into output and history; reconstruct a processor to start fresh.
Spectrum `reset` also clears partial input and sums. Power sums accumulate in
`f32` until reset, and the segment count saturates at `usize::MAX`.

## Consumers and status

[thd75-listen](https://github.com/swiftraccoon/kenwood/tree/main/thd75-listen)
owns command-line tuning, capture and playback.
[azimuth-core](https://github.com/swiftraccoon/kenwood/tree/main/azimuth-core)
uses the channelizer and spectrum estimator for its native app.
Consumers import `if_dsp::Complex32` to use the same public complex type.

The crate is unpublished and experimental; its public API may change.
Part of the [kenwood workspace](https://github.com/swiftraccoon/kenwood).
License: GPL-2.0-or-later.
