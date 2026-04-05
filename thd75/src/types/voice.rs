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
//! TH-D75 user manual. Derived from the capability gap analysis
//! feature 145.

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
    /// Channel number (1-4).
    pub channel: VoiceChannel,
    /// Channel name (up to 8 characters).
    pub name: VoiceMessageName,
    /// Recorded duration in seconds (0 = empty/no recording).
    pub duration_secs: u8,
    /// Enable repeat playback.
    pub repeat: bool,
    /// Repeat playback interval in seconds (0-60).
    pub repeat_interval: RepeatInterval,
}

impl Default for VoiceMessage {
    fn default() -> Self {
        Self {
            channel: VoiceChannel::Ch1,
            name: VoiceMessageName::default(),
            duration_secs: 0,
            repeat: false,
            repeat_interval: RepeatInterval::default(),
        }
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

/// Voice message name (up to 8 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct VoiceMessageName(String);

impl VoiceMessageName {
    /// Maximum length of a voice message name.
    pub const MAX_LEN: usize = 8;

    /// Creates a new voice message name.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 8 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
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
    /// Returns `None` if the value exceeds 60 seconds.
    #[must_use]
    pub const fn new(seconds: u8) -> Option<Self> {
        if seconds <= Self::MAX {
            Some(Self(seconds))
        } else {
            None
        }
    }

    /// Returns the interval in seconds.
    #[must_use]
    pub const fn seconds(self) -> u8 {
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
    fn voice_message_default() {
        let msg = VoiceMessage::default();
        assert_eq!(msg.channel, VoiceChannel::Ch1);
        assert_eq!(msg.duration_secs, 0);
        assert!(!msg.repeat);
    }

    #[test]
    fn voice_message_name_valid() {
        let name = VoiceMessageName::new("CQ Call").unwrap();
        assert_eq!(name.as_str(), "CQ Call");
    }

    #[test]
    fn voice_message_name_max_length() {
        let name = VoiceMessageName::new("12345678").unwrap();
        assert_eq!(name.as_str().len(), 8);
    }

    #[test]
    fn voice_message_name_too_long() {
        assert!(VoiceMessageName::new("123456789").is_none());
    }

    #[test]
    fn repeat_interval_valid_range() {
        assert!(RepeatInterval::new(0).is_some());
        assert!(RepeatInterval::new(30).is_some());
        assert!(RepeatInterval::new(60).is_some());
    }

    #[test]
    fn repeat_interval_invalid() {
        assert!(RepeatInterval::new(61).is_none());
    }

    #[test]
    fn repeat_interval_value() {
        let interval = RepeatInterval::new(45).unwrap();
        assert_eq!(interval.seconds(), 45);
    }

    #[test]
    fn repeat_interval_default() {
        let interval = RepeatInterval::default();
        assert_eq!(interval.seconds(), 0);
    }
}
