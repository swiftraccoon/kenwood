//! Channel records carried by `FO` (a band's current settings) and `ME`
//! (a stored memory channel).
//!
//! The 18 shared fields were read from firmware 1.02 in VFO, Memory and DR
//! tuning modes on both bands; the two fields `ME` adds were written and read
//! back on stored channels, and the same channels' image records matched.

use std::fmt;

use crate::types::{
    CrossToneField, DcsCode, DigitalCode, DigitalSquelch, Frequency, OperatingMode, ShiftDirection,
    StepSize, ToneCode, ToneMode, TransmitSetting, UrCallsign,
};

/// Number of comma-separated fields after the band or channel selector.
pub const CHANNEL_FIELD_COUNT: usize = 18;

/// The 18 channel fields shared by `FO` and `ME`, in wire order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatChannelRecord {
    /// Receive frequency.
    pub receive_frequency: Frequency,
    /// Repeater offset, or the transmit frequency of a split channel.
    pub transmit: TransmitSetting,
    /// Receive tuning step.
    pub receive_step: StepSize,
    /// Transmit tuning step; equal to the receive step on a VFO record.
    pub transmit_step: StepSize,
    /// Demodulation mode.
    pub mode: OperatingMode,
    /// The single active tone-signaling function.
    pub tone_mode: ToneMode,
    /// Whether reverse (listening on the transmit frequency) is on.
    pub reverse: bool,
    /// Offset direction.
    pub shift: ShiftDirection,
    /// Transmit tone table index.
    pub tone_code: ToneCode,
    /// Receive CTCSS table index.
    pub ctcss_code: ToneCode,
    /// DCS table index.
    pub dcs_code: DcsCode,
    /// The cross-tone field, retained exactly.
    pub cross_tone: CrossToneField,
    /// D-STAR destination callsign.
    pub ur_call: UrCallsign,
    /// D-STAR digital squelch type.
    pub digital_squelch: DigitalSquelch,
    /// D-STAR digital code.
    pub digital_code: DigitalCode,
}

impl CatChannelRecord {
    /// The 18 fields in wire order, comma separated.
    #[must_use]
    pub fn to_wire_fields(&self) -> String {
        self.wire_fields().join(",")
    }

    /// The 18 fields in wire order.
    #[must_use]
    pub fn wire_fields(&self) -> [String; CHANNEL_FIELD_COUNT] {
        let [tone, ctcss, dcs, cross] = self.tone_mode.to_flags();
        [
            self.receive_frequency.to_wire_string(),
            self.transmit.to_wire_string(),
            self.receive_step.wire_char().to_string(),
            self.transmit_step.wire_char().to_string(),
            u8::from(self.mode).to_string(),
            u8::from(tone).to_string(),
            u8::from(ctcss).to_string(),
            u8::from(dcs).to_string(),
            u8::from(cross).to_string(),
            u8::from(self.reverse).to_string(),
            self.shift.as_raw().to_string(),
            format!("{:02}", self.tone_code.as_raw()),
            format!("{:02}", self.ctcss_code.as_raw()),
            format!("{:03}", self.dcs_code.as_raw()),
            self.cross_tone.wire_char().to_string(),
            self.ur_call.as_str().to_owned(),
            self.digital_squelch.as_raw().to_string(),
            format!("{:02}", self.digital_code.as_raw()),
        ]
    }
}

impl fmt::Display for CatChannelRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} {} step {}",
            self.receive_frequency, self.mode, self.shift, self.receive_step
        )?;
        match self.transmit {
            TransmitSetting::Offset(offset) if self.shift != ShiftDirection::Simplex => {
                write!(formatter, " offset {offset}")?;
            }
            TransmitSetting::Split(frequency) => write!(formatter, " split TX {frequency}")?,
            TransmitSetting::Offset(_) => {}
        }
        match self.tone_mode {
            ToneMode::Off => {}
            ToneMode::Tone => write!(formatter, " tone {}", self.tone_code)?,
            ToneMode::Ctcss => write!(formatter, " CTCSS {}", self.ctcss_code)?,
            ToneMode::Dcs => write!(formatter, " DCS {}", self.dcs_code)?,
            ToneMode::CrossTone => write!(formatter, " cross tone {}", self.cross_tone)?,
        }
        if self.reverse {
            formatter.write_str(" reverse")?;
        }
        Ok(())
    }
}

/// A stored memory channel as `ME` reports and accepts it: the shared record
/// with the split flag inserted after the reverse flag and the scan lockout
/// flag appended.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatMemoryChannelRecord {
    /// The shared channel fields.
    pub channel: CatChannelRecord,
    /// Whether the transmit field is an independent split frequency.
    pub split: bool,
    /// Whether memory scan skips the channel.
    pub scan_lockout: bool,
}

impl CatMemoryChannelRecord {
    /// The 20 fields after the address in `ME` wire order, comma separated.
    #[must_use]
    pub fn to_wire_fields(&self) -> String {
        let shared = self.channel.wire_fields();
        let (before_split, after_split) = shared.split_at(10);
        let mut fields: Vec<&str> = before_split.iter().map(String::as_str).collect();
        let split = u8::from(self.split).to_string();
        let lockout = u8::from(self.scan_lockout).to_string();
        fields.push(&split);
        fields.extend(after_split.iter().map(String::as_str));
        fields.push(&lockout);
        fields.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OffsetFrequency;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn repeater() -> Result<CatChannelRecord, Box<dyn std::error::Error>> {
        Ok(CatChannelRecord {
            receive_frequency: Frequency::new(145_190_000)?,
            transmit: TransmitSetting::Offset(OffsetFrequency::new(600_000)?),
            receive_step: StepSize::Hz5000,
            transmit_step: StepSize::Hz5000,
            mode: OperatingMode::Fm,
            tone_mode: ToneMode::Off,
            reverse: false,
            shift: ShiftDirection::Minus,
            tone_code: ToneCode::new(8)?,
            ctcss_code: ToneCode::new(8)?,
            dcs_code: DcsCode::new(0)?,
            cross_tone: CrossToneField::new(0)?,
            ur_call: UrCallsign::new("CQCQCQ")?,
            digital_squelch: DigitalSquelch::Off,
            digital_code: DigitalCode::new(0)?,
        })
    }

    #[test]
    fn wire_fields_reproduce_the_observed_vfo_record() -> TestResult {
        assert_eq!(
            repeater()?.to_wire_fields(),
            "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00"
        );
        let memory = CatMemoryChannelRecord {
            channel: repeater()?,
            split: true,
            scan_lockout: true,
        };
        assert_eq!(
            memory.to_wire_fields(),
            "0145190000,0000600000,2,2,0,0,0,0,0,0,1,2,08,08,000,0,CQCQCQ,0,00,1"
        );
        Ok(())
    }

    #[test]
    fn display_summarizes_the_operating_settings() -> TestResult {
        let mut record = repeater()?;
        assert_eq!(
            record.to_string(),
            "145.190000 MHz FM - step 5 kHz offset 0.600000 MHz"
        );
        record.tone_mode = ToneMode::Ctcss;
        record.reverse = true;
        assert_eq!(
            record.to_string(),
            "145.190000 MHz FM - step 5 kHz offset 0.600000 MHz CTCSS 88.5 Hz reverse"
        );
        record.shift = ShiftDirection::Simplex;
        record.transmit = TransmitSetting::Split(Frequency::new(446_000_000)?);
        record.tone_mode = ToneMode::Off;
        record.reverse = false;
        assert_eq!(
            record.to_string(),
            "145.190000 MHz FM simplex step 5 kHz split TX 446.000000 MHz"
        );
        Ok(())
    }
}
