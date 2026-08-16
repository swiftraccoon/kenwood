//! Sans-io D-STAR echo test unit.
//!
//! An echo test records one incoming voice stream and replays it to the
//! sender after a short pause, per the ircDDBGateway `EchoUnit`
//! convention: a stream is an echo request when its UR field is
//! `"       E"` (or when the host arms the unit for the next stream),
//! and the playback header answers as the station itself with the
//! `ECHO` suffix and `CQCQCQ` destination, preserving the original
//! flags and repeater fields.
//!
//! The unit is sans-io like the rest of this crate: feed it stream
//! events with an injected `now`, then poll [`EchoUnit::poll_playback`]
//! until it hands over the recorded stream exactly once. Frame pacing
//! and transmission belong to the caller.

use std::time::{Duration, Instant};

use crate::header::DstarHeader;
use crate::types::{Callsign, Suffix};
use crate::voice::VoiceFrame;

/// Default recording cap: 60 seconds at 50 frames per second.
pub const DEFAULT_MAX_FRAMES: usize = 60 * 50;

/// Default pause between the recorded stream's end and playback,
/// per ircDDBGateway `REPLY_TIME`.
pub const DEFAULT_REPLY_DELAY: Duration = Duration::from_millis(500);

/// The UR field value that requests an echo test.
const ECHO_UR: [u8; 8] = *b"       E";

/// The `MY` suffix used on echo playback.
const ECHO_SUFFIX: [u8; 4] = *b"ECHO";

/// A recorded stream ready to transmit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoPlayback {
    /// Playback header: `MY` = the station, suffix `ECHO`,
    /// UR `CQCQCQ`, original flags and repeater fields preserved.
    pub header: DstarHeader,
    /// The recorded AMBE frames, in receive order.
    pub frames: Vec<VoiceFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Idle,
    Recording {
        header: DstarHeader,
        frames: Vec<VoiceFrame>,
    },
    Waiting {
        header: DstarHeader,
        frames: Vec<VoiceFrame>,
        since: Instant,
    },
}

/// Sans-io echo recorder and replay scheduler.
///
/// See the [module documentation](self) for the protocol convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoUnit {
    state: State,
    armed: bool,
    max_frames: usize,
    reply_delay: Duration,
}

impl Default for EchoUnit {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoUnit {
    /// Create a unit with [`DEFAULT_MAX_FRAMES`] and
    /// [`DEFAULT_REPLY_DELAY`].
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_FRAMES, DEFAULT_REPLY_DELAY)
    }

    /// Create a unit with explicit recording and delay limits.
    #[must_use]
    pub const fn with_limits(max_frames: usize, reply_delay: Duration) -> Self {
        Self {
            state: State::Idle,
            armed: false,
            max_frames,
            reply_delay,
        }
    }

    /// Whether a stream header requests an echo test by UR convention.
    #[must_use]
    pub fn is_echo_request(header: &DstarHeader) -> bool {
        header.ur_call.as_bytes() == &ECHO_UR
    }

    /// Arm the unit: the next stream records regardless of its UR field.
    pub const fn arm(&mut self) {
        self.armed = true;
    }

    /// Whether the unit is currently armed for the next stream.
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// Whether the unit is idle: not recording and not holding a
    /// recording for playback. Arming alone leaves the unit idle.
    #[must_use]
    pub const fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Whether a stream is currently being recorded.
    #[must_use]
    pub const fn is_recording(&self) -> bool {
        matches!(self.state, State::Recording { .. })
    }

    /// Begin recording if this stream is an echo request or the unit
    /// is armed. Returns whether recording started; arming is consumed
    /// either way once a stream starts.
    pub fn on_voice_start(&mut self, header: &DstarHeader) -> bool {
        let wanted = self.armed || Self::is_echo_request(header);
        self.armed = false;
        if wanted {
            self.state = State::Recording {
                header: *header,
                frames: Vec::new(),
            };
        }
        wanted
    }

    /// Record one voice frame; silently ignored when not recording or
    /// once the recording cap is reached.
    pub fn on_voice_frame(&mut self, frame: VoiceFrame) {
        if let State::Recording { frames, .. } = &mut self.state
            && frames.len() < self.max_frames
        {
            frames.push(frame);
        }
    }

    /// End the stream: a recording transitions to the waiting window.
    /// Returns the number of recorded frames, or `None` when the unit
    /// was not recording.
    pub fn on_voice_end(&mut self, now: Instant) -> Option<usize> {
        let state = std::mem::replace(&mut self.state, State::Idle);
        if let State::Recording { header, frames } = state {
            let count = frames.len();
            self.state = State::Waiting {
                header,
                frames,
                since: now,
            };
            Some(count)
        } else {
            self.state = state;
            None
        }
    }

    /// Hand over the recorded stream once the reply delay has elapsed.
    ///
    /// Returns `Some` exactly once per recording; `station` becomes the
    /// playback header's `MY` field. The caller transmits the header,
    /// paces the frames, and sends the EOT.
    pub fn poll_playback(&mut self, now: Instant, station: Callsign) -> Option<EchoPlayback> {
        let ready = matches!(
            &self.state,
            State::Waiting { since, .. }
                if now.saturating_duration_since(*since) >= self.reply_delay
        );
        if !ready {
            return None;
        }
        let state = std::mem::replace(&mut self.state, State::Idle);
        let State::Waiting { header, frames, .. } = state else {
            unreachable!("readiness was just matched on Waiting");
        };
        Some(EchoPlayback {
            header: DstarHeader {
                flag1: header.flag1,
                flag2: header.flag2,
                flag3: header.flag3,
                rpt2: header.rpt2,
                rpt1: header.rpt1,
                ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
                my_call: station,
                my_suffix: Suffix::from_wire_bytes(ECHO_SUFFIX),
            },
            frames,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_header() -> DstarHeader {
        DstarHeader {
            flag1: 0x40,
            flag2: 0x01,
            flag3: 0x02,
            rpt2: Callsign::from_wire_bytes(*b"XRF001 G"),
            rpt1: Callsign::from_wire_bytes(*b"XRF001 B"),
            ur_call: Callsign::from_wire_bytes(ECHO_UR),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
    }

    #[test]
    fn ur_convention_and_arming_both_start_recording() {
        let mut unit = EchoUnit::new();
        assert!(EchoUnit::is_echo_request(&request_header()));
        assert!(unit.on_voice_start(&request_header()));

        let mut unit = EchoUnit::new();
        let mut plain = request_header();
        plain.ur_call = Callsign::from_wire_bytes(*b"CQCQCQ  ");
        assert!(!unit.on_voice_start(&plain), "plain streams are ignored");
        unit.arm();
        assert!(unit.on_voice_start(&plain), "arming overrides UR");
        assert!(!unit.is_armed(), "arming is consumed by the stream");
    }

    #[test]
    fn playback_waits_for_the_reply_delay_and_fires_once() {
        let mut unit = EchoUnit::with_limits(10, Duration::from_millis(500));
        let station = Callsign::from_wire_bytes(*b"KQ4NIT  ");
        let t0 = Instant::now();

        assert!(unit.is_idle());
        assert!(unit.on_voice_start(&request_header()));
        assert!(unit.is_recording());
        unit.on_voice_frame(VoiceFrame::silence());
        unit.on_voice_frame(VoiceFrame::silence());
        assert_eq!(unit.on_voice_end(t0), Some(2));
        assert!(
            !unit.is_idle() && !unit.is_recording(),
            "a finished recording waits for playback"
        );

        assert!(
            unit.poll_playback(t0 + Duration::from_millis(499), station)
                .is_none(),
            "playback must wait out the reply delay"
        );
        let playback = unit
            .poll_playback(t0 + Duration::from_millis(500), station)
            .unwrap_or_else(|| unreachable!("delay elapsed"));
        assert_eq!(playback.frames.len(), 2);
        assert_eq!(playback.header.my_call, station);
        assert_eq!(playback.header.my_suffix, Suffix::from_wire_bytes(*b"ECHO"));
        assert_eq!(
            playback.header.ur_call,
            Callsign::from_wire_bytes(*b"CQCQCQ  ")
        );
        // Original flags and repeater fields ride along unchanged.
        assert_eq!(playback.header.flag1, 0x40);
        assert_eq!(
            playback.header.rpt1,
            Callsign::from_wire_bytes(*b"XRF001 B")
        );

        assert!(unit.is_idle(), "handover returns the unit to idle");
        assert!(
            unit.poll_playback(t0 + Duration::from_secs(2), station)
                .is_none(),
            "playback hands over exactly once"
        );
    }

    #[test]
    fn recording_caps_at_the_frame_limit() {
        let mut unit = EchoUnit::with_limits(3, DEFAULT_REPLY_DELAY);
        assert!(unit.on_voice_start(&request_header()));
        for _ in 0..10 {
            unit.on_voice_frame(VoiceFrame::silence());
        }
        assert_eq!(
            unit.on_voice_end(Instant::now()),
            Some(3),
            "frames beyond the cap are dropped"
        );
    }

    #[test]
    fn end_without_recording_reports_none() {
        let mut unit = EchoUnit::new();
        assert_eq!(unit.on_voice_end(Instant::now()), None);
    }
}
