//! The full IF-to-audio pipeline.

use crate::agc::{Agc, AgcConfig};
use crate::demod::{AmDemod, CwDemod, SsbDemod};
use crate::nco::Nco;
use crate::resample::{Decimator, Interpolator};
use crate::{BASEBAND_RATE, Complex32, IF_CENTER_HZ, INPUT_RATE, RATE_FACTOR};

/// Demodulation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemodMode {
    /// Upper sideband.
    Usb,
    /// Lower sideband.
    Lsb,
    /// CW (narrow USB centered on a 700 Hz sidetone).
    Cw,
    /// Amplitude modulation (envelope detection).
    Am,
}

impl DemodMode {
    /// Default audio passband width for the mode, in hertz.
    #[must_use]
    pub const fn default_filter_hz(self) -> f32 {
        match self {
            Self::Usb | Self::Lsb => 2_600.0,
            Self::Cw => 500.0,
            Self::Am => 4_500.0,
        }
    }
}

impl std::fmt::Display for DemodMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usb => f.write_str("USB"),
            Self::Lsb => f.write_str("LSB"),
            Self::Cw => f.write_str("CW"),
            Self::Am => f.write_str("AM"),
        }
    }
}

/// Channelizer construction parameters.
#[derive(Debug, Clone, Copy)]
pub struct ChannelizerConfig {
    /// Demodulation mode.
    pub mode: DemodMode,
    /// Audio passband width override in hertz (`None` = mode default).
    pub filter_hz: Option<f32>,
    /// AGC parameters (`None` disables AGC — used by amplitude-precise
    /// tests; keep it on for listening).
    pub agc: Option<AgcConfig>,
}

impl Default for ChannelizerConfig {
    fn default() -> Self {
        Self {
            mode: DemodMode::Usb,
            filter_hz: None,
            agc: Some(AgcConfig::default()),
        }
    }
}

/// SSB passband low edge in hertz (keeps hum and the carrier slot out).
const SSB_LOW_EDGE_HZ: f32 = 100.0;
/// CW sidetone pitch in hertz.
const CW_PITCH_HZ: f32 = 700.0;
/// Taps for the decimation anti-alias lowpass (at the input rate).
const DECIM_TAPS: usize = 63;
/// Taps for the mode passband (at the baseband rate).
const MODE_TAPS: usize = 255;
/// Taps for the interpolation image filter (at the output rate).
const INTERP_TAPS: usize = 63;

enum ModeDemod {
    Ssb(SsbDemod),
    Cw(CwDemod),
    Am(AmDemod),
}

impl std::fmt::Debug for ModeDemod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ssb(_) => f.write_str("ModeDemod::Ssb"),
            Self::Cw(_) => f.write_str("ModeDemod::Cw"),
            Self::Am(_) => f.write_str("ModeDemod::Am"),
        }
    }
}

fn build_demod(mode: DemodMode, filter_hz: f32) -> ModeDemod {
    match mode {
        DemodMode::Usb => ModeDemod::Ssb(SsbDemod::new(
            SSB_LOW_EDGE_HZ,
            SSB_LOW_EDGE_HZ + filter_hz,
            BASEBAND_RATE,
            MODE_TAPS,
        )),
        DemodMode::Lsb => ModeDemod::Ssb(SsbDemod::new(
            -(SSB_LOW_EDGE_HZ + filter_hz),
            -SSB_LOW_EDGE_HZ,
            BASEBAND_RATE,
            MODE_TAPS,
        )),
        DemodMode::Cw => ModeDemod::Cw(CwDemod::new(
            CW_PITCH_HZ,
            filter_hz,
            BASEBAND_RATE,
            MODE_TAPS,
        )),
        DemodMode::Am => ModeDemod::Am(AmDemod::new(filter_hz, BASEBAND_RATE, DECIM_TAPS)),
    }
}

/// IF stream in, demodulated audio out. See the crate docs for the
/// pipeline stages and buffer conventions.
#[derive(Debug)]
pub struct Channelizer {
    nco: Nco,
    decim: Decimator,
    demod: ModeDemod,
    agc: Option<Agc>,
    interp: Interpolator,
    mode: DemodMode,
    filter_hz: f32,
    mixed: Vec<Complex32>,
    baseband: Vec<Complex32>,
    audio: Vec<f32>,
}

impl Channelizer {
    /// Build the pipeline for [`INPUT_RATE`] input centered on
    /// [`IF_CENTER_HZ`].
    #[must_use]
    pub fn new(config: ChannelizerConfig) -> Self {
        let filter_hz = config
            .filter_hz
            .unwrap_or_else(|| config.mode.default_filter_hz());
        Self {
            nco: Nco::new(-f64::from(IF_CENTER_HZ), f64::from(INPUT_RATE)),
            decim: Decimator::new(
                RATE_FACTOR,
                BASEBAND_RATE / 2.0 * 0.9,
                INPUT_RATE,
                DECIM_TAPS,
            ),
            demod: build_demod(config.mode, filter_hz),
            agc: config.agc.map(|c| Agc::new(c, BASEBAND_RATE)),
            interp: Interpolator::new(
                RATE_FACTOR,
                BASEBAND_RATE / 2.0 * 0.9,
                crate::OUTPUT_RATE,
                INTERP_TAPS,
            ),
            mode: config.mode,
            filter_hz,
            mixed: Vec::new(),
            baseband: Vec::new(),
            audio: Vec::new(),
        }
    }

    /// Current demodulation mode.
    #[must_use]
    pub const fn mode(&self) -> DemodMode {
        self.mode
    }

    /// Current audio passband width in hertz.
    #[must_use]
    pub const fn filter_hz(&self) -> f32 {
        self.filter_hz
    }

    /// Switch demodulation mode. The mode path is rebuilt with the
    /// mode's default passband; mixer, decimator, and AGC state carry
    /// over so audio resumes immediately.
    pub fn set_mode(&mut self, mode: DemodMode) {
        self.mode = mode;
        self.filter_hz = mode.default_filter_hz();
        self.demod = build_demod(mode, self.filter_hz);
    }

    /// Override (or reset, with `None`) the audio passband width.
    pub fn set_filter_hz(&mut self, hz: Option<f32>) {
        self.filter_hz = hz.unwrap_or_else(|| self.mode.default_filter_hz());
        self.demod = build_demod(self.mode, self.filter_hz);
    }

    /// Process one block of IF samples into demodulated audio at the
    /// output rate. `out` is cleared first. Arbitrary block sizes are
    /// fine; stream state carries across calls.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.mixed.clear();
        for &x in input {
            self.mixed.push(self.nco.next_sample() * x);
        }
        self.decim.process(&self.mixed, &mut self.baseband);
        match &mut self.demod {
            ModeDemod::Ssb(d) => d.process(&self.baseband, &mut self.audio),
            ModeDemod::Cw(d) => d.process(&self.baseband, &mut self.audio),
            ModeDemod::Am(d) => d.process(&self.baseband, &mut self.audio),
        }
        if let Some(agc) = &mut self.agc {
            agc.process(&mut self.audio);
        }
        self.interp.process(&self.audio, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum_test_support::tone_amplitude;

    /// Real IF tone at `IF_CENTER_HZ + offset_hz`, unit amplitude.
    fn if_tone(offset_hz: f64, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| {
                #[expect(clippy::cast_precision_loss, reason = "small test lengths")]
                let ph = std::f64::consts::TAU * (12_000.0 + offset_hz) * n as f64 / 48_000.0;
                #[expect(clippy::cast_possible_truncation, reason = "unit tone samples")]
                let s = ph.cos() as f32;
                s
            })
            .collect()
    }

    fn no_agc(mode: DemodMode) -> Channelizer {
        Channelizer::new(ChannelizerConfig {
            mode,
            filter_hz: None,
            agc: None,
        })
    }

    #[test]
    fn usb_tone_above_center_demodulates_at_offset() {
        let mut ch = no_agc(DemodMode::Usb);
        let mut out = Vec::new();
        ch.process(&if_tone(1_000.0, 96_000), &mut out);
        let steady = out.get(24_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0);
        assert!((amp - 1.0).abs() < 0.1, "USB amplitude {amp}, want ~1.0");
        let below = tone_amplitude(steady, 48_000.0, 2_000.0);
        assert!(below < 0.05, "spurious 2 kHz content {below}");
    }

    #[test]
    fn usb_rejects_a_tone_below_center() {
        let mut ch = no_agc(DemodMode::Usb);
        let mut out = Vec::new();
        ch.process(&if_tone(-1_000.0, 96_000), &mut out);
        let steady = out.get(24_000..).unwrap_or(&[]);
        let leak = tone_amplitude(steady, 48_000.0, 1_000.0);
        assert!(leak < 0.05, "opposite-sideband leakage {leak}");
    }

    #[test]
    fn lsb_tone_below_center_demodulates_at_offset() {
        let mut ch = no_agc(DemodMode::Lsb);
        let mut out = Vec::new();
        ch.process(&if_tone(-1_000.0, 96_000), &mut out);
        let steady = out.get(24_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0);
        assert!((amp - 1.0).abs() < 0.1, "LSB amplitude {amp}, want ~1.0");
    }

    #[test]
    fn mode_and_filter_switch_midstream_without_disruption() {
        let mut ch = no_agc(DemodMode::Usb);
        let mut out = Vec::new();
        ch.process(&if_tone(1_000.0, 48_000), &mut out);
        ch.set_mode(DemodMode::Lsb);
        assert_eq!(ch.mode(), DemodMode::Lsb);
        assert!(
            (ch.filter_hz() - DemodMode::Lsb.default_filter_hz()).abs() < f32::EPSILON,
            "mode switch resets the filter width"
        );
        ch.process(&if_tone(-1_000.0, 96_000), &mut out);
        let steady = out.get(24_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0);
        assert!((amp - 1.0).abs() < 0.15, "post-switch amplitude {amp}");
        ch.set_filter_hz(Some(1_800.0));
        assert!((ch.filter_hz() - 1_800.0).abs() < f32::EPSILON);
        ch.process(&if_tone(-1_000.0, 48_000), &mut out);
        assert_eq!(out.len(), 48_000, "output length tracks input length");
    }

    #[test]
    fn agc_path_produces_target_level_audio() {
        let mut ch = Channelizer::new(ChannelizerConfig::default());
        let mut out = Vec::new();
        // Quiet IF tone: AGC must lift it toward the 0.25 target.
        let quiet: Vec<f32> = if_tone(1_000.0, 144_000).iter().map(|s| s * 0.02).collect();
        ch.process(&quiet, &mut out);
        let steady = out.get(96_000..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0);
        assert!(
            amp > 0.1 && amp < 0.4,
            "AGC output amplitude {amp}, want near 0.25"
        );
    }
}
