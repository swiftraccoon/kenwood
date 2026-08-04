//! Voice message memory types.
//!
//! The TH-D75 provides 4 voice message memory channels for recording
//! and playing back short audio messages. Channel 1 supports up to
//! 30 seconds of audio; channels 2-4 support up to 15 seconds each.
//! Recorded messages can be transmitted, played back locally, or set
//! to repeat at a configurable interval.
//!
//! Per User Manual Chapter 22 and the menu table:
//!
//! - Menu No. 310: Voice message list.
//! - Menu No. 311: TX monitor (Off/On, default: On) -- hear your own
//!   transmitted voice message through the speaker.
//! - Menu No. 312: Digital auto reply -- automatically reply to D-STAR
//!   calls with a voice message (Off / Voice Message 1-4, default: Off).
//!
//! These types model voice message settings from Chapter 22 of the
//! TH-D75 user manual.

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Voice message channel
// ---------------------------------------------------------------------------

/// Voice message memory channel.
///
/// The TH-D75 has 4 voice message channels:
/// - Channel 1: up to 30 seconds recording
/// - Channels 2-4: up to 15 seconds recording each
///
/// Messages can be recorded from the microphone, played back through
/// the speaker, transmitted on air, or cleared individually.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VoiceMessage {
    channel: VoiceChannel,
    name: VoiceMessageName,
    duration_secs: u8,
    repeat: bool,
    repeat_interval: RepeatInterval,
}

impl VoiceMessage {
    /// Creates a voice-message record whose duration fits its channel.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::VoiceMessageDurationOutOfRange`] when the
    /// duration exceeds 30 seconds for channel 1 or 15 seconds for channels
    /// 2 through 4.
    pub fn new(
        channel: VoiceChannel,
        name: VoiceMessageName,
        duration_secs: u8,
        repeat: bool,
        repeat_interval: RepeatInterval,
    ) -> Result<Self, ValidationError> {
        let maximum = channel.max_duration_secs();
        if duration_secs > maximum {
            return Err(ValidationError::VoiceMessageDurationOutOfRange {
                channel: channel.number(),
                seconds: duration_secs,
                maximum,
            });
        }
        Ok(Self {
            channel,
            name,
            duration_secs,
            repeat,
            repeat_interval,
        })
    }

    /// Returns the voice-message channel.
    #[must_use]
    pub const fn channel(&self) -> VoiceChannel {
        self.channel
    }

    /// Returns the validated message name.
    #[must_use]
    pub const fn name(&self) -> &VoiceMessageName {
        &self.name
    }

    /// Returns the recorded duration in seconds, or zero for an empty slot.
    #[must_use]
    pub const fn duration_secs(&self) -> u8 {
        self.duration_secs
    }

    /// Returns whether repeat playback is enabled.
    #[must_use]
    pub const fn repeat(&self) -> bool {
        self.repeat
    }

    /// Returns the repeat interval.
    #[must_use]
    pub const fn repeat_interval(&self) -> RepeatInterval {
        self.repeat_interval
    }
}

/// Voice message channel number (1-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoiceChannel {
    /// Channel 1 (up to 30 seconds).
    Ch1,
    /// Channel 2 (up to 15 seconds).
    Ch2,
    /// Channel 3 (up to 15 seconds).
    Ch3,
    /// Channel 4 (up to 15 seconds).
    Ch4,
}

impl VoiceChannel {
    /// Returns the 1-based channel number.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Ch1 => 1,
            Self::Ch2 => 2,
            Self::Ch3 => 3,
            Self::Ch4 => 4,
        }
    }

    /// Returns the maximum recording duration in seconds for this channel.
    #[must_use]
    pub const fn max_duration_secs(self) -> u8 {
        match self {
            Self::Ch1 => 30,
            Self::Ch2 | Self::Ch3 | Self::Ch4 => 15,
        }
    }
}

/// Voice message name (up to 8 UTF-8 encoded bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct VoiceMessageName(String);

impl VoiceMessageName {
    /// Maximum length of a voice message name.
    pub const MAX_LEN: usize = 8;

    /// Creates a new voice message name.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::VoiceMessageNameTooLong`] if `text` exceeds
    /// eight UTF-8 encoded bytes.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        if text.len() <= Self::MAX_LEN {
            Ok(Self(text.to_owned()))
        } else {
            Err(ValidationError::VoiceMessageNameTooLong { len: text.len() })
        }
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Repeat playback interval in seconds (0-60).
///
/// When repeat playback is enabled, the voice message replays after
/// waiting for the configured interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RepeatInterval(u8);

impl RepeatInterval {
    /// Maximum repeat interval in seconds.
    pub const MAX: u8 = 60;

    /// Creates a new repeat interval.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::VoiceRepeatIntervalOutOfRange`] if the value
    /// exceeds 60 seconds.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds <= Self::MAX {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::VoiceRepeatIntervalOutOfRange { seconds })
        }
    }

    /// Returns the interval in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_channel_numbers() {
        assert_eq!(VoiceChannel::Ch1.number(), 1);
        assert_eq!(VoiceChannel::Ch2.number(), 2);
        assert_eq!(VoiceChannel::Ch3.number(), 3);
        assert_eq!(VoiceChannel::Ch4.number(), 4);
    }

    #[test]
    fn voice_channel_max_durations() {
        assert_eq!(VoiceChannel::Ch1.max_duration_secs(), 30);
        assert_eq!(VoiceChannel::Ch2.max_duration_secs(), 15);
        assert_eq!(VoiceChannel::Ch3.max_duration_secs(), 15);
        assert_eq!(VoiceChannel::Ch4.max_duration_secs(), 15);
    }

    #[test]
    fn voice_message_duration_tracks_channel_capacity() -> Result<(), ValidationError> {
        let name = VoiceMessageName::new("CQ")?;
        let repeat_interval = RepeatInterval::new(10)?;
        let ch1 = VoiceMessage::new(VoiceChannel::Ch1, name.clone(), 30, true, repeat_interval)?;
        assert_eq!(ch1.duration_secs(), 30);
        assert!(ch1.repeat());
        assert_eq!(ch1.name(), &name);
        assert_eq!(ch1.repeat_interval(), repeat_interval);

        assert!(matches!(
            VoiceMessage::new(VoiceChannel::Ch2, name, 16, false, repeat_interval),
            Err(ValidationError::VoiceMessageDurationOutOfRange {
                channel: 2,
                seconds: 16,
                maximum: 15,
            })
        ));
        Ok(())
    }

    #[test]
    fn voice_message_name_valid() -> Result<(), Box<dyn std::error::Error>> {
        let name = VoiceMessageName::new("CQ Call")?;
        assert_eq!(name.as_str(), "CQ Call");
        Ok(())
    }

    #[test]
    fn voice_message_name_max_length() -> Result<(), Box<dyn std::error::Error>> {
        let name = VoiceMessageName::new("12345678")?;
        assert_eq!(name.as_str().len(), 8);
        Ok(())
    }

    #[test]
    fn voice_message_name_too_long() {
        assert!(matches!(
            VoiceMessageName::new("123456789"),
            Err(ValidationError::VoiceMessageNameTooLong { len: 9 })
        ));
    }

    #[test]
    fn repeat_interval_valid_range() {
        assert!(RepeatInterval::new(0).is_ok());
        assert!(RepeatInterval::new(30).is_ok());
        assert!(RepeatInterval::new(60).is_ok());
    }

    #[test]
    fn repeat_interval_invalid() {
        assert!(matches!(
            RepeatInterval::new(61),
            Err(ValidationError::VoiceRepeatIntervalOutOfRange { seconds: 61 })
        ));
    }

    #[test]
    fn repeat_interval_value() -> Result<(), Box<dyn std::error::Error>> {
        let interval = RepeatInterval::new(45)?;
        assert_eq!(interval.as_seconds(), 45);
        Ok(())
    }

    #[test]
    fn repeat_interval_default() {
        let interval = RepeatInterval::default();
        assert_eq!(interval.as_seconds(), 0);
    }
}
