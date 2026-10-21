//! Four named protocol deadlines, driven by injected instants.

use std::time::Instant;

/// One independently armed session timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TimerId {
    Keepalive,
    KeepaliveInactivity,
    DisconnectDeadline,
    VoiceInactivity,
}

/// Fixed timer storage without allocation or string-based timer identities.
#[derive(Debug, Default)]
pub(crate) struct TimerWheel {
    keepalive: Option<Instant>,
    keepalive_inactivity: Option<Instant>,
    disconnect_deadline: Option<Instant>,
    voice_inactivity: Option<Instant>,
}

impl TimerWheel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) const fn set(&mut self, timer: TimerId, deadline: Instant) {
        *self.timer_mut(timer) = Some(deadline);
    }

    pub(crate) const fn clear(&mut self, timer: TimerId) {
        *self.timer_mut(timer) = None;
    }

    pub(crate) fn is_expired(&self, timer: TimerId, now: Instant) -> bool {
        self.timer(timer).is_some_and(|deadline| now >= deadline)
    }

    /// Earliest armed deadline, including a deadline already due.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        [
            self.keepalive,
            self.keepalive_inactivity,
            self.disconnect_deadline,
            self.voice_inactivity,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    const fn timer(&self, timer: TimerId) -> Option<Instant> {
        match timer {
            TimerId::Keepalive => self.keepalive,
            TimerId::KeepaliveInactivity => self.keepalive_inactivity,
            TimerId::DisconnectDeadline => self.disconnect_deadline,
            TimerId::VoiceInactivity => self.voice_inactivity,
        }
    }

    const fn timer_mut(&mut self, timer: TimerId) -> &mut Option<Instant> {
        match timer {
            TimerId::Keepalive => &mut self.keepalive,
            TimerId::KeepaliveInactivity => &mut self.keepalive_inactivity,
            TimerId::DisconnectDeadline => &mut self.disconnect_deadline,
            TimerId::VoiceInactivity => &mut self.voice_inactivity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timers_replace_independently_and_expired_deadlines_remain_visible()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = Instant::now();
        let early = now
            .checked_add(Duration::from_secs(1))
            .ok_or("early deadline")?;
        let late = now
            .checked_add(Duration::from_secs(2))
            .ok_or("late deadline")?;
        let mut timers = TimerWheel::new();
        assert!(timers.next_deadline().is_none());
        timers.set(TimerId::Keepalive, late);
        timers.set(TimerId::KeepaliveInactivity, early);
        timers.set(TimerId::DisconnectDeadline, late);
        timers.set(TimerId::VoiceInactivity, early);
        assert_eq!(timers.next_deadline(), Some(early));
        assert!(!timers.is_expired(TimerId::Keepalive, early));
        assert!(timers.is_expired(TimerId::VoiceInactivity, early));
        timers.clear(TimerId::KeepaliveInactivity);
        assert_eq!(
            timers.next_deadline(),
            Some(early),
            "clearing one timer preserves the other deadline"
        );
        timers.set(TimerId::VoiceInactivity, late);
        assert_eq!(timers.next_deadline(), Some(late));
        for timer in [
            TimerId::Keepalive,
            TimerId::KeepaliveInactivity,
            TimerId::DisconnectDeadline,
            TimerId::VoiceInactivity,
        ] {
            timers.clear(timer);
        }
        assert!(timers.next_deadline().is_none());
        Ok(())
    }
}
