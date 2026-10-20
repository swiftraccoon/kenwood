//! Swift-facing live IF DSP processor.

use std::sync::{Arc, Mutex, MutexGuard};

use if_dsp::{
    Bandwidth, Channelizer, ChannelizerConfig, DemodMode, DspError, FftSize, INPUT_RATE,
    SpectrumEstimator,
};

/// FFT size used by the live IF spectrum.
const SPECTRUM_FFT_SIZE: FftSize = match FftSize::new(1_024) {
    Ok(size) => size,
    Err(_) => unreachable!(),
};
/// Publish roughly ten spectra per second at the required 48 kHz input rate.
const SPECTRUM_INTERVAL_SAMPLES: u64 = 4_800;
/// Hann coherent gain, expressed as the sum of a 1,024-sample Hann window.
const SPECTRUM_WINDOW_SUM: f32 = 512.0;
/// The lowest displayed level. This also represents mathematical silence.
const LEVEL_FLOOR_DBFS: f32 = -120.0;
/// Protect the FFI boundary from accidental unbounded allocations.
const MAX_INPUT_SAMPLES: usize = 48_000;
/// Safe passband bounds below the 6 kHz complex-baseband Nyquist limit.
const MIN_FILTER_HZ: f32 = 100.0;
const MAX_FILTER_HZ: f32 = 5_500.0;

/// Demodulator selected for the 12 kHz low-IF stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum IfDspMode {
    /// Upper sideband.
    Usb,
    /// Lower sideband.
    Lsb,
    /// CW with a 700 Hz sidetone.
    Cw,
    /// Amplitude modulation.
    Am,
}

impl From<IfDspMode> for DemodMode {
    fn from(value: IfDspMode) -> Self {
        match value {
            IfDspMode::Usb => Self::Usb,
            IfDspMode::Lsb => Self::Lsb,
            IfDspMode::Cw => Self::Cw,
            IfDspMode::Am => Self::Am,
        }
    }
}

/// Operator-controlled IF demodulator configuration.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct IfDspConfiguration {
    /// Demodulation mode.
    pub mode: IfDspMode,
    /// Passband width in hertz. `None` selects the mode default.
    pub filter_hz: Option<f32>,
}

/// A calibrated input spectrum derived from physical PCM samples.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct IfDspSpectrum {
    /// Frequency offset of the first bin from the 12 kHz IF center.
    pub first_bin_offset_hz: f32,
    /// Distance between adjacent bins.
    pub bin_width_hz: f32,
    /// One-sided Hann-windowed amplitude spectrum in dBFS.
    pub levels_dbfs: Vec<f32>,
    /// Strongest-bin offset from the 12 kHz IF center.
    pub peak_offset_hz: f32,
    /// Strongest-bin amplitude in dBFS.
    pub peak_level_dbfs: f32,
}

/// Result of processing one block of real 48 kHz mono IF PCM.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct IfDspFrame {
    /// Increasing block sequence since construction or reset, saturating at `u64::MAX`.
    pub sequence: u64,
    /// Total physical input samples processed since construction or reset.
    pub input_sample_count: u64,
    /// Smoothed input RMS level in dBFS.
    pub input_level_dbfs: f32,
    /// Smoothed demodulated-output RMS level in dBFS.
    pub output_level_dbfs: f32,
    /// Latest spectrum when its ten-hertz publication interval elapsed.
    /// An absent value means the caller should retain its previous spectrum.
    pub spectrum: Option<IfDspSpectrum>,
    /// Cumulative accepted input samples at full scale (`-1` or `1`).
    pub clipped_sample_count: u64,
}

/// Input or configuration rejected at the DSP boundary.
#[derive(Debug, Clone, PartialEq, thiserror::Error, uniffi::Error)]
pub enum IfDspError {
    /// A passband was outside the safe DSP range.
    #[error("IF DSP filter must be between 100 and 5500 Hz; received {filter_hz}")]
    InvalidFilter {
        /// Rejected passband width.
        filter_hz: f32,
    },
    /// The checked DSP pipeline could not construct or replace its configuration.
    #[error("IF DSP configuration failed: {reason}")]
    DspConfiguration {
        /// Scalar, passband, coefficient or timing failure from the DSP library.
        reason: String,
    },
    /// A PCM sample was NaN or infinite.
    #[error("IF DSP PCM sample {sample_index} was not finite")]
    NonFiniteSample {
        /// Index within the rejected block.
        sample_index: u64,
    },
    /// A finite PCM sample exceeded the normalized full-scale range.
    #[error("IF DSP PCM sample {sample_index} is outside -1..=1")]
    SampleOutOfRange {
        /// Index within the rejected block.
        sample_index: u64,
    },
    /// A single FFI call exceeded the bounded block contract.
    #[error("IF DSP block has {sample_count} samples; maximum is 48000")]
    BlockTooLarge {
        /// Rejected block length.
        sample_count: u64,
    },
    /// The processor mutex was poisoned by a prior foreign panic.
    #[error("IF DSP processor state is unavailable")]
    StateUnavailable,
}

#[derive(Debug)]
struct ProcessorState {
    channelizer: Channelizer,
    spectrum: SpectrumEstimator,
    configuration: IfDspConfiguration,
    sequence: u64,
    input_sample_count: u64,
    samples_since_spectrum: u64,
    clipped_sample_count: u64,
    smoothed_input_rms: f32,
    smoothed_output_rms: f32,
    output: Vec<f32>,
    spectrum_power: Vec<f32>,
}

impl ProcessorState {
    fn new(configuration: IfDspConfiguration) -> Result<Self, IfDspError> {
        Ok(Self {
            channelizer: build_channelizer(configuration)?,
            spectrum: SpectrumEstimator::new(SPECTRUM_FFT_SIZE),
            configuration,
            sequence: 0,
            input_sample_count: 0,
            samples_since_spectrum: 0,
            clipped_sample_count: 0,
            smoothed_input_rms: 0.0,
            smoothed_output_rms: 0.0,
            output: Vec::new(),
            spectrum_power: Vec::new(),
        })
    }

    fn set_configuration(&mut self, configuration: IfDspConfiguration) -> Result<(), IfDspError> {
        let checked = checked_configuration(configuration)?;
        let mut replacement = self.channelizer.clone();
        if configuration.mode != self.configuration.mode {
            replacement
                .set_mode(checked.mode())
                .map_err(configuration_error)?;
        }
        replacement
            .set_filter_width(Some(checked.filter_width()))
            .map_err(configuration_error)?;
        self.channelizer = replacement;
        self.configuration = configuration;
        Ok(())
    }

    fn process(&mut self, samples: &[f32]) -> IfDspFrame {
        self.sequence = self.sequence.saturating_add(1);
        self.input_sample_count = self.input_sample_count.saturating_add(samples.len() as u64);
        self.samples_since_spectrum = self
            .samples_since_spectrum
            .saturating_add(samples.len() as u64);
        self.clipped_sample_count = self
            .clipped_sample_count
            .saturating_add(samples.iter().filter(|sample| sample.abs() >= 1.0).count() as u64);

        self.spectrum.feed(samples);
        self.channelizer.process(samples, &mut self.output);

        self.smoothed_input_rms = smooth_rms(self.smoothed_input_rms, rms(samples));
        self.smoothed_output_rms = smooth_rms(self.smoothed_output_rms, rms(&self.output));

        let spectrum = if self.samples_since_spectrum >= SPECTRUM_INTERVAL_SAMPLES {
            self.samples_since_spectrum = 0;
            let rendered = self.render_spectrum();
            self.spectrum.reset();
            Some(rendered)
        } else {
            None
        };

        IfDspFrame {
            sequence: self.sequence,
            input_sample_count: self.input_sample_count,
            input_level_dbfs: amplitude_dbfs(self.smoothed_input_rms),
            output_level_dbfs: amplitude_dbfs(self.smoothed_output_rms),
            spectrum,
            clipped_sample_count: self.clipped_sample_count,
        }
    }

    fn render_spectrum(&mut self) -> IfDspSpectrum {
        self.spectrum.write_bin_power(&mut self.spectrum_power);
        let mut levels_dbfs = Vec::with_capacity(self.spectrum_power.len());
        for (index, power) in self.spectrum_power.iter().copied().enumerate() {
            let one_sided_scale = if index == 0 || index + 1 == self.spectrum_power.len() {
                1.0
            } else {
                2.0
            };
            let amplitude = one_sided_scale * power.sqrt() / SPECTRUM_WINDOW_SUM;
            levels_dbfs.push(amplitude_dbfs(amplitude));
        }

        let (peak_index, peak_level_dbfs) = levels_dbfs
            .iter()
            .copied()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .unwrap_or((0, LEVEL_FLOOR_DBFS));
        let bin_width_hz = INPUT_RATE / 1_024.0;
        let first_bin_offset_hz = -if_dsp::IF_CENTER_HZ;
        #[expect(
            clippy::cast_precision_loss,
            reason = "the peak index is bounded to the 513-bin spectrum"
        )]
        let peak_offset_hz = (peak_index as f32).mul_add(bin_width_hz, first_bin_offset_hz);
        IfDspSpectrum {
            first_bin_offset_hz,
            bin_width_hz,
            levels_dbfs,
            peak_offset_hz,
            peak_level_dbfs,
        }
    }

    fn reset(&mut self) -> Result<(), IfDspError> {
        let configuration = self.configuration;
        let replacement = build_channelizer(configuration)?;
        self.channelizer = replacement;
        self.spectrum.reset();
        self.sequence = 0;
        self.input_sample_count = 0;
        self.samples_since_spectrum = 0;
        self.clipped_sample_count = 0;
        self.smoothed_input_rms = 0.0;
        self.smoothed_output_rms = 0.0;
        self.output.clear();
        self.spectrum_power.clear();
        Ok(())
    }
}

/// Thread-safe sans-I/O IF processor used by the Apple audio-capture service.
#[derive(Debug, uniffi::Object)]
pub struct IfDspProcessor {
    state: Mutex<ProcessorState>,
}

#[uniffi::export]
impl IfDspProcessor {
    /// Construct a processor for normalized, real, mono, 48 kHz PCM.
    ///
    /// # Errors
    ///
    /// Returns [`IfDspError::InvalidFilter`] for an unsafe passband or
    /// [`IfDspError::DspConfiguration`] for a checked DSP setup failure.
    #[uniffi::constructor]
    pub fn new(configuration: IfDspConfiguration) -> Result<Arc<Self>, IfDspError> {
        Ok(Arc::new(Self {
            state: Mutex::new(ProcessorState::new(configuration)?),
        }))
    }

    /// Return the active operator configuration.
    ///
    /// # Errors
    ///
    /// Returns [`IfDspError::StateUnavailable`] if the processor is poisoned.
    pub fn configuration(self: Arc<Self>) -> Result<IfDspConfiguration, IfDspError> {
        Ok(lock(&self.state)?.configuration)
    }

    /// Apply a mode and passband change without resetting stream counters.
    ///
    /// # Errors
    ///
    /// Returns an invalid-filter, DSP-configuration or unavailable-state error.
    /// Rejection preserves configuration, measurements and stream history.
    pub fn set_configuration(
        self: Arc<Self>,
        configuration: IfDspConfiguration,
    ) -> Result<(), IfDspError> {
        lock(&self.state)?.set_configuration(configuration)
    }

    /// Process one physical PCM block and return live measurements.
    ///
    /// Input must be finite, real, mono, 48 kHz PCM within `-1..=1`. The Apple
    /// integration converts the selected USB input to that format before
    /// calling this method and never calls it from the real-time audio thread.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized block, non-finite or out-of-range PCM,
    /// or poisoned processor state. Rejection preserves the complete stream
    /// state. Counters saturate at `u64::MAX`; reset starts them at zero.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "UniFFI transfers sequences as owned vectors across the foreign boundary"
    )]
    pub fn process_pcm(self: Arc<Self>, samples: Vec<f32>) -> Result<IfDspFrame, IfDspError> {
        if samples.len() > MAX_INPUT_SAMPLES {
            return Err(IfDspError::BlockTooLarge {
                sample_count: samples.len() as u64,
            });
        }
        if let Some(sample_index) = samples.iter().position(|sample| !sample.is_finite()) {
            return Err(IfDspError::NonFiniteSample {
                sample_index: sample_index as u64,
            });
        }
        if let Some(sample_index) = samples.iter().position(|sample| sample.abs() > 1.0) {
            return Err(IfDspError::SampleOutOfRange {
                sample_index: sample_index as u64,
            });
        }
        Ok(lock(&self.state)?.process(&samples))
    }

    /// Reset DSP history, measurements, and counters while retaining config.
    ///
    /// # Errors
    ///
    /// Returns [`IfDspError::StateUnavailable`] if the processor is poisoned or
    /// [`IfDspError::DspConfiguration`] if rebuilding the DSP pipeline fails.
    /// Failure preserves the previous processor and counters.
    pub fn reset(self: Arc<Self>) -> Result<(), IfDspError> {
        lock(&self.state)?.reset()
    }
}

fn lock(state: &Mutex<ProcessorState>) -> Result<MutexGuard<'_, ProcessorState>, IfDspError> {
    state.lock().map_err(|_| IfDspError::StateUnavailable)
}

fn validate_configuration(configuration: IfDspConfiguration) -> Result<(), IfDspError> {
    if let Some(filter_hz) = configuration.filter_hz
        && (!filter_hz.is_finite() || !(MIN_FILTER_HZ..=MAX_FILTER_HZ).contains(&filter_hz))
    {
        return Err(IfDspError::InvalidFilter { filter_hz });
    }
    Ok(())
}

fn checked_configuration(
    configuration: IfDspConfiguration,
) -> Result<ChannelizerConfig, IfDspError> {
    validate_configuration(configuration)?;
    let mut checked = ChannelizerConfig::new(configuration.mode.into());
    if let Some(hz) = configuration.filter_hz {
        checked = checked
            .with_filter_width(Bandwidth::new(hz).map_err(configuration_error)?)
            .map_err(configuration_error)?;
    }
    Ok(checked)
}

fn build_channelizer(configuration: IfDspConfiguration) -> Result<Channelizer, IfDspError> {
    Channelizer::new(checked_configuration(configuration)?).map_err(configuration_error)
}

fn configuration_error(error: DspError) -> IfDspError {
    IfDspError::DspConfiguration {
        reason: error.to_string(),
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares = samples.iter().fold(0.0_f64, |sum, sample| {
        f64::from(*sample).mul_add(f64::from(*sample), sum)
    });
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "audio blocks are bounded to 48000 samples and normalized RMS fits f32"
    )]
    let result = (sum_squares / samples.len() as f64).sqrt() as f32;
    result
}

fn smooth_rms(previous: f32, current: f32) -> f32 {
    if previous == 0.0 {
        current
    } else {
        0.2_f32.mul_add(current, 0.8 * previous)
    }
}

fn amplitude_dbfs(amplitude: f32) -> f32 {
    if amplitude > 1.0e-6 {
        (20.0 * amplitude.log10()).max(LEVEL_FLOOR_DBFS)
    } else {
        LEVEL_FLOOR_DBFS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(mode: IfDspMode) -> IfDspConfiguration {
        IfDspConfiguration {
            mode,
            filter_hz: None,
        }
    }

    fn tone(frequency_hz: f64, sample_count: usize) -> Vec<f32> {
        (0..sample_count)
            .map(|index| {
                #[expect(clippy::cast_precision_loss, reason = "small test sample counts")]
                let phase =
                    std::f64::consts::TAU * frequency_hz * index as f64 / f64::from(INPUT_RATE);
                #[expect(clippy::cast_possible_truncation, reason = "unit sine sample")]
                let sample = phase.sin() as f32;
                sample
            })
            .collect()
    }

    #[test]
    fn physical_if_tone_produces_calibrated_spectrum_and_output_level() -> Result<(), IfDspError> {
        let processor = IfDspProcessor::new(configuration(IfDspMode::Usb))?;
        let frame = processor.process_pcm(tone(13_000.0, 9_600))?;
        let spectrum = frame.spectrum.ok_or(IfDspError::StateUnavailable)?;

        assert_eq!(frame.sequence, 1);
        assert_eq!(frame.input_sample_count, 9_600);
        assert!(
            frame.output_level_dbfs.is_finite(),
            "demodulated output level"
        );
        assert!(
            (spectrum.peak_offset_hz - 1_000.0).abs() < 100.0,
            "peak offset {}",
            spectrum.peak_offset_hz
        );
        assert!(
            spectrum.peak_level_dbfs > -4.0,
            "full-scale tone peak {} dBFS",
            spectrum.peak_level_dbfs
        );
        assert!(
            (-3.2..=-2.8).contains(&frame.input_level_dbfs),
            "sine RMS {} dBFS",
            frame.input_level_dbfs
        );
        Ok(())
    }

    #[test]
    fn spectrum_is_rate_limited_and_reset_clears_counters() -> Result<(), IfDspError> {
        let processor = IfDspProcessor::new(configuration(IfDspMode::Am))?;
        let first = processor.clone().process_pcm(vec![0.0; 2_400])?;
        assert!(first.spectrum.is_none(), "first half interval");
        let second = processor.clone().process_pcm(vec![0.0; 2_400])?;
        assert!(second.spectrum.is_some(), "completed interval");
        processor.clone().reset()?;
        let reset = processor.process_pcm(vec![0.0; 64])?;
        assert_eq!(reset.sequence, 1);
        assert_eq!(reset.input_sample_count, 64);
        assert_eq!(reset.clipped_sample_count, 0);
        Ok(())
    }

    #[test]
    fn configuration_changes_and_invalid_input_are_explicit() -> Result<(), IfDspError> {
        let processor = IfDspProcessor::new(configuration(IfDspMode::Usb))?;
        processor.clone().set_configuration(IfDspConfiguration {
            mode: IfDspMode::Cw,
            filter_hz: Some(400.0),
        })?;
        assert_eq!(
            processor.clone().configuration()?,
            IfDspConfiguration {
                mode: IfDspMode::Cw,
                filter_hz: Some(400.0),
            }
        );
        assert!(matches!(
            processor.clone().set_configuration(IfDspConfiguration {
                mode: IfDspMode::Usb,
                filter_hz: Some(6_000.0),
            }),
            Err(IfDspError::InvalidFilter { .. })
        ));
        assert!(matches!(
            processor.process_pcm(vec![f32::NAN]),
            Err(IfDspError::NonFiniteSample { sample_index: 0 })
        ));
        Ok(())
    }

    #[test]
    fn rejected_pcm_preserves_counters_and_filter_history() -> Result<(), IfDspError> {
        let changed = IfDspProcessor::new(configuration(IfDspMode::Usb))?;
        let original = IfDspProcessor::new(configuration(IfDspMode::Usb))?;
        let prefix = tone(13_000.0, 137);
        assert_eq!(
            changed.clone().process_pcm(prefix.clone())?,
            original.clone().process_pcm(prefix)?
        );
        let rejected = changed.clone().process_pcm(vec![0.0, f32::MAX]);
        assert!(
            matches!(
                rejected,
                Err(IfDspError::SampleOutOfRange { sample_index: 1 })
            ),
            "{rejected:?}"
        );
        let suffix = tone(13_000.0, 129);
        assert_eq!(
            changed.process_pcm(suffix.clone())?,
            original.process_pcm(suffix)?
        );
        Ok(())
    }

    #[test]
    fn rejected_combined_mode_and_width_change_preserves_measurements_and_history()
    -> Result<(), IfDspError> {
        let config = configuration(IfDspMode::Usb);
        let mut changed = ProcessorState::new(config)?;
        let mut original = ProcessorState::new(config)?;
        let prefix = tone(13_000.0, 137);
        assert_eq!(
            changed.process(&prefix),
            original.process(&prefix),
            "initial state"
        );
        let rejected = changed.set_configuration(IfDspConfiguration {
            mode: IfDspMode::Lsb,
            filter_hz: Some(6_000.0),
        });
        assert!(
            matches!(rejected, Err(IfDspError::InvalidFilter { .. })),
            "{rejected:?}"
        );
        assert_eq!(
            changed.configuration, original.configuration,
            "operator configuration"
        );
        assert_eq!(
            changed.channelizer.mode(),
            original.channelizer.mode(),
            "active demodulator"
        );
        let suffix = tone(13_000.0, 129);
        assert_eq!(
            changed.process(&suffix),
            original.process(&suffix),
            "measurement counters and levels"
        );
        let actual: Vec<_> = changed
            .output
            .iter()
            .map(|sample| sample.to_bits())
            .collect();
        let expected: Vec<_> = original
            .output
            .iter()
            .map(|sample| sample.to_bits())
            .collect();
        assert_eq!(
            actual, expected,
            "rejected changes preserve audio history exactly"
        );
        Ok(())
    }
}
