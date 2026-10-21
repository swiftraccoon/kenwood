// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Bounded worker commands, coalesced status and independent terminal control.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::AudioStatus;
use super::error::AudioError;

/// Maximum retained failures between GUI drains; excess failures are counted.
const ERROR_CAPACITY: usize = 32;

/// A bounded status snapshot shared by the audio thread and GUI.
#[derive(Debug, Clone, Default)]
pub(crate) struct AudioMailbox(Arc<Mutex<MailboxState>>);

#[derive(Debug, Default)]
struct MailboxState {
    latest: Vec<AudioStatus>,
    errors: VecDeque<AudioError>,
    lost_errors: u64,
}

impl AudioMailbox {
    /// Replace a preceding observation of the same kind, or retain a failure.
    pub(super) fn send(&self, status: AudioStatus) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let AudioStatus::Error(error) = status {
            if state.errors.len() == ERROR_CAPACITY {
                let _oldest = state.errors.pop_front();
                state.lost_errors = state.lost_errors.saturating_add(1);
            }
            state.errors.push_back(error);
        } else {
            let kind = std::mem::discriminant(&status);
            if let Some(previous) = state
                .latest
                .iter_mut()
                .find(|item| std::mem::discriminant(*item) == kind)
            {
                *previous = status;
            } else {
                state.latest.push(status);
            }
        }
    }

    /// Take the latest observations and retained failures, including loss count.
    pub(crate) fn drain(&self) -> Vec<AudioStatus> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut output = std::mem::take(&mut state.latest);
        let lost = std::mem::take(&mut state.lost_errors);
        if lost > 0 {
            output.push(AudioStatus::Error(AudioError::DiagnosticOverflow(lost)));
        }
        output.extend(state.errors.drain(..).map(AudioStatus::Error));
        output
    }
}

/// Stop and shutdown requests have dedicated storage outside the command queue.
#[derive(Debug, Default)]
pub(super) struct AudioControl {
    pub(super) shutdown: AtomicBool,
    pub(super) identity: Mutex<Option<dstar_gateway_core::types::RoutingCallsign>>,
    pub(super) stop_generation: AtomicU64,
    pub(super) recording_generation: AtomicU64,
    pub(super) playback_generation: AtomicU64,
}

impl AudioControl {
    pub(super) fn stop(&self) {
        let _previous = self.stop_generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// Unique responsibility for requesting cleanup and joining the audio thread.
#[derive(Debug)]
pub(crate) struct AudioWorkerOwner {
    pub(super) control: Arc<AudioControl>,
    pub(super) worker: JoinHandle<()>,
}

impl AudioWorkerOwner {
    /// Request termination, finalize recording and join the worker explicitly.
    ///
    /// Device backends and file sync may delay completion. This method must run
    /// after the GUI exits, never on a destructor or the asynchronous runtime.
    pub(crate) fn shutdown(self) -> std::thread::Result<()> {
        self.control.shutdown.store(true, Ordering::Release);
        self.worker.join()
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioMailbox, ERROR_CAPACITY};
    use crate::audio::AudioStatus;
    use crate::audio::error::AudioError;

    #[test]
    fn explicit_shutdown_observes_worker_completion_before_returning()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let control = Arc::new(super::AudioControl::default());
        let thread_control = Arc::clone(&control);
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = Arc::clone(&finished);
        let worker = std::thread::spawn(move || {
            while !thread_control.shutdown.load(Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            thread_finished.store(true, Ordering::Release);
        });
        let owner = super::AudioWorkerOwner { control, worker };
        owner.shutdown().map_err(|_| "audio test worker panicked")?;
        assert!(
            finished.load(Ordering::Acquire),
            "shutdown returned before worker completion"
        );
        Ok(())
    }

    #[test]
    fn status_coalesces_and_diagnostic_overflow_is_counted() {
        let mailbox = AudioMailbox::default();
        for _ in 0..1000 {
            mailbox.send(AudioStatus::Recording(true));
            mailbox.send(AudioStatus::Error(AudioError::QueueFull));
        }
        let observations = mailbox.drain();
        assert_eq!(
            observations.len(),
            ERROR_CAPACITY + 2,
            "status storage was unbounded"
        );
        assert!(
            observations.iter().any(|item| matches!(
                item,
                AudioStatus::Error(AudioError::DiagnosticOverflow(968))
            )),
            "overflow count was hidden"
        );
        assert!(mailbox.drain().is_empty(), "drain repeated observations");
    }
}
