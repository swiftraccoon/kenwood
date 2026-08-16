//! Sans-io radio-state monitor over AI push notifications.
//!
//! With `AI 1` enabled the radio pushes `BY`/`FQ`/`MD`/`SQ` changes
//! unsolicited. Consumers were each hand-rolling the same fold: keep a
//! per-band snapshot current from those pushes, and apply the BY-gate
//! S-meter policy (`SM` must never be polled blind; a fresh reading is
//! warranted only when the squelch opens, and a closed squelch means a
//! zero meter). [`StateMonitor`] owns that fold and that policy without
//! performing any I/O: feed it every broadcast [`Response`] via
//! [`StateMonitor::apply`], service the returned [`StateChange`] (the
//! only one needing radio I/O is [`StateChange::BecameBusy`], answered
//! with [`StateMonitor::apply_smeter`]), and read coherent snapshots
//! via [`StateMonitor::band`].

use crate::protocol::Response;
use crate::types::{Band, Frequency, OperatingMode, SMeterReading, SquelchLevel};

/// Coherent snapshot of AI-observable state for one band.
///
/// Every field starts `None` and becomes `Some` once the first push
/// (or serviced read) for it arrives; `None` therefore means "not yet
/// observed", never "known empty".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BandState {
    /// Receive frequency, from pushed `FQ`.
    pub frequency: Option<Frequency>,
    /// Operating mode, from pushed `MD`.
    pub mode: Option<OperatingMode>,
    /// Squelch level, from pushed `SQ`.
    pub squelch: Option<SquelchLevel>,
    /// Channel-busy state, from pushed `BY`.
    pub busy: Option<bool>,
    /// S-meter reading: pushed `SM`, a serviced
    /// [`StateChange::BecameBusy`] read, or the zero the BY-gate
    /// applies when the squelch closes.
    pub s_meter: Option<SMeterReading>,
}

/// What one applied [`Response`] changed in the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateChange {
    /// The band's receive frequency updated.
    Frequency {
        /// Band whose state changed.
        band: Band,
    },
    /// The band's operating mode updated.
    Mode {
        /// Band whose state changed.
        band: Band,
    },
    /// The band's squelch level updated.
    Squelch {
        /// Band whose state changed.
        band: Band,
    },
    /// The band's squelch opened. This is the BY-gate: the caller
    /// should perform one `SM` read and report it via
    /// [`StateMonitor::apply_smeter`]. `SM` must never be polled
    /// outside this window (the firmware returns spurious spikes).
    BecameBusy {
        /// Band whose squelch opened.
        band: Band,
    },
    /// The band's squelch closed. The monitor has already zeroed the
    /// S-meter; no radio I/O is warranted.
    BecameIdle {
        /// Band whose squelch closed.
        band: Band,
    },
    /// The band's S-meter reading updated from a pushed `SM`.
    SMeter {
        /// Band whose state changed.
        band: Band,
    },
}

/// Sans-io fold of AI push notifications into per-band snapshots.
///
/// See the [module documentation](self) for the usage pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateMonitor {
    band_a: BandState,
    band_b: BandState,
}

impl StateMonitor {
    /// Create a monitor with every field unobserved.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            band_a: BandState {
                frequency: None,
                mode: None,
                squelch: None,
                busy: None,
                s_meter: None,
            },
            band_b: BandState {
                frequency: None,
                mode: None,
                squelch: None,
                busy: None,
                s_meter: None,
            },
        }
    }

    /// The current snapshot for `band`.
    #[must_use]
    pub const fn band(&self, band: Band) -> &BandState {
        match band {
            Band::A => &self.band_a,
            Band::B => &self.band_b,
        }
    }

    const fn band_mut(&mut self, band: Band) -> &mut BandState {
        match band {
            Band::A => &mut self.band_a,
            Band::B => &mut self.band_b,
        }
    }

    /// Fold one broadcast response into the snapshot.
    ///
    /// Returns what changed, or `None` for responses that carry no
    /// monitored state. Feed every message from
    /// [`Radio::subscribe`](crate::radio::Radio::subscribe) here.
    pub const fn apply(&mut self, response: &Response) -> Option<StateChange> {
        match *response {
            Response::Frequency { band, frequency } => {
                self.band_mut(band).frequency = Some(frequency);
                Some(StateChange::Frequency { band })
            }
            Response::OperatingMode { band, mode } => {
                self.band_mut(band).mode = Some(mode);
                Some(StateChange::Mode { band })
            }
            Response::Squelch { band, level } => {
                self.band_mut(band).squelch = Some(level);
                Some(StateChange::Squelch { band })
            }
            Response::Smeter { band, level } => {
                self.band_mut(band).s_meter = Some(level);
                Some(StateChange::SMeter { band })
            }
            Response::Busy { band, busy } => {
                let state = self.band_mut(band);
                state.busy = Some(busy);
                if busy {
                    Some(StateChange::BecameBusy { band })
                } else {
                    // Closed squelch means a zero meter with no poll.
                    state.s_meter = Some(SMeterReading::ZERO);
                    Some(StateChange::BecameIdle { band })
                }
            }
            _ => None,
        }
    }

    /// Record the `SM` reading taken to service a
    /// [`StateChange::BecameBusy`].
    pub const fn apply_smeter(&mut self, band: Band, reading: SMeterReading) {
        self.band_mut(band).s_meter = Some(reading);
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::Response;
    use crate::types::{Band, Frequency, OperatingMode, RadioModel, SMeterReading, SquelchLevel};

    use super::{StateChange, StateMonitor};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn busy_open_requests_smeter_and_close_zeroes_it() -> TestResult {
        let mut monitor = StateMonitor::new();

        let change = monitor.apply(&Response::Busy {
            band: Band::A,
            busy: true,
        });
        assert_eq!(change, Some(StateChange::BecameBusy { band: Band::A }));
        assert_eq!(monitor.band(Band::A).busy, Some(true));

        // The caller services the change with one gated SM read.
        monitor.apply_smeter(Band::A, SMeterReading::new(4)?);
        assert_eq!(monitor.band(Band::A).s_meter, Some(SMeterReading::new(4)?));

        let change = monitor.apply(&Response::Busy {
            band: Band::A,
            busy: false,
        });
        assert_eq!(change, Some(StateChange::BecameIdle { band: Band::A }));
        assert_eq!(monitor.band(Band::A).busy, Some(false));
        assert_eq!(
            monitor.band(Band::A).s_meter,
            Some(SMeterReading::ZERO),
            "a closed squelch means a zero meter, no SM poll"
        );
        Ok(())
    }

    #[test]
    fn pushed_frequency_mode_and_squelch_fold_into_the_snapshot() -> TestResult {
        let mut monitor = StateMonitor::new();

        let change = monitor.apply(&Response::Frequency {
            band: Band::B,
            frequency: Frequency::new(145_240_000),
        });
        assert_eq!(change, Some(StateChange::Frequency { band: Band::B }));
        assert_eq!(
            monitor.band(Band::B).frequency,
            Some(Frequency::new(145_240_000))
        );

        let change = monitor.apply(&Response::OperatingMode {
            band: Band::B,
            mode: OperatingMode::Fm,
        });
        assert_eq!(change, Some(StateChange::Mode { band: Band::B }));
        assert_eq!(monitor.band(Band::B).mode, Some(OperatingMode::Fm));

        let change = monitor.apply(&Response::Squelch {
            band: Band::B,
            level: SquelchLevel::new(3)?,
        });
        assert_eq!(change, Some(StateChange::Squelch { band: Band::B }));
        assert_eq!(monitor.band(Band::B).squelch, Some(SquelchLevel::new(3)?));

        // Band A stays untouched by band-B pushes.
        assert_eq!(monitor.band(Band::A).frequency, None);
        Ok(())
    }

    #[test]
    fn pushed_smeter_folds_and_unrelated_responses_are_ignored() -> TestResult {
        let mut monitor = StateMonitor::new();

        let change = monitor.apply(&Response::Smeter {
            band: Band::A,
            level: SMeterReading::new(2)?,
        });
        assert_eq!(change, Some(StateChange::SMeter { band: Band::A }));
        assert_eq!(monitor.band(Band::A).s_meter, Some(SMeterReading::new(2)?));

        let change = monitor.apply(&Response::RadioId {
            model: RadioModel::ThD75,
        });
        assert_eq!(change, None, "non-state responses fold to nothing");
        Ok(())
    }
}
