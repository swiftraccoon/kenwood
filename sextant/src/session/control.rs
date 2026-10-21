// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Bounded presentation events and terminal requests independent of voice load.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use super::{SessionCommand, SessionEvent};

/// Ordinary session commands plus a dedicated disconnect/shutdown register.
#[derive(Debug, Clone)]
pub(crate) struct SessionHandle {
    commands: mpsc::Sender<SessionCommand>,
    control: watch::Sender<ControlState>,
}

/// A terminal request generation; shutdown remains set once requested.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ControlState {
    pub(super) generation: u64,
    pub(super) shutdown: bool,
}

impl SessionHandle {
    /// Create the bounded operation channel and its independent terminal path.
    pub(crate) fn channel() -> (
        Self,
        mpsc::Receiver<SessionCommand>,
        watch::Receiver<ControlState>,
    ) {
        let (commands, receiver) = mpsc::channel(32);
        let (control, terminal) = watch::channel(ControlState::default());
        (Self { commands, control }, receiver, terminal)
    }

    pub(crate) fn try_send(
        &self,
        command: SessionCommand,
    ) -> Result<(), mpsc::error::TrySendError<SessionCommand>> {
        self.commands.try_send(command)
    }

    pub(crate) fn audio_sender(&self) -> mpsc::Sender<SessionCommand> {
        self.commands.clone()
    }

    /// Supersede queued operations and cancel any in-progress connection.
    pub(crate) fn disconnect(&self) {
        self.control
            .send_modify(|state| state.generation = state.generation.wrapping_add(1));
    }

    /// Request disconnect and completion; capacity cannot reject this request.
    pub(crate) fn shutdown(&self) {
        self.control.send_modify(|state| {
            state.generation = state.generation.wrapping_add(1);
            state.shutdown = true;
        });
    }
}

/// Maximum retained application events; audio frames use their own queue.
const EVENT_CAPACITY: usize = 1024;

/// Bounded GUI observations with coalesced current status and explicit loss.
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionEvents(Arc<Mutex<EventState>>);

#[derive(Debug, Default)]
struct EventState {
    events: VecDeque<SessionEvent>,
    lost: u64,
}

impl SessionEvents {
    /// Retain an event without allowing a paused GUI to block session control.
    pub(super) fn send(
        &self,
        event: SessionEvent,
    ) -> std::future::Ready<Result<(), std::convert::Infallible>> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replace = matches!(
            event,
            SessionEvent::Status(_)
                | SessionEvent::RxStats { .. }
                | SessionEvent::LinkHealth { .. }
        );
        if replace {
            let kind = std::mem::discriminant(&event);
            state
                .events
                .retain(|previous| std::mem::discriminant(previous) != kind);
        }
        if state.events.len() == EVENT_CAPACITY {
            let disposable = state
                .events
                .iter()
                .position(|item| !matches!(item, SessionEvent::Status(_)));
            if let Some(index) = disposable {
                let _discarded = state.events.remove(index);
                state.lost = state.lost.saturating_add(1);
            }
        }
        state.events.push_back(event);
        drop(state);
        std::future::ready(Ok(()))
    }

    /// Take one retained event; overflow is reported before the remaining data.
    pub(crate) fn try_recv(&self) -> Result<SessionEvent, mpsc::error::TryRecvError> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lost = std::mem::take(&mut state.lost);
        if lost > 0 {
            return Ok(SessionEvent::Error(format!(
                "GUI event queue exceeded capacity; {lost} observations lost"
            )));
        }
        state
            .events
            .pop_front()
            .ok_or(mpsc::error::TryRecvError::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::{EVENT_CAPACITY, SessionEvents, SessionHandle};
    use crate::session::{ConnStatus, SessionCommand, SessionEvent};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn terminal_requests_survive_a_full_command_queue() -> TestResult {
        let (handle, _receiver, terminal) = SessionHandle::channel();
        for _ in 0..32 {
            handle.try_send(SessionCommand::EndTx)?;
        }
        assert!(
            handle.try_send(SessionCommand::EndTx).is_err(),
            "queue did not fill"
        );
        handle.disconnect();
        assert_eq!(
            terminal.borrow().generation,
            1,
            "disconnect was lost behind audio"
        );
        handle.shutdown();
        assert!(terminal.borrow().shutdown, "shutdown was lost behind audio");
        Ok(())
    }

    #[tokio::test]
    async fn events_remain_bounded_and_preserve_current_status() -> TestResult {
        let events = SessionEvents::default();
        events
            .send(SessionEvent::Status(ConnStatus::Disconnecting))
            .await?;
        for index in 0..2000 {
            events.send(SessionEvent::Log(index.to_string())).await?;
        }
        assert!(
            matches!(events.try_recv()?, SessionEvent::Error(message) if message.contains("977")),
            "overflow was hidden"
        );
        assert!(
            matches!(
                events.try_recv()?,
                SessionEvent::Status(ConnStatus::Disconnecting)
            ),
            "current status was evicted"
        );
        let mut count = 1;
        while events.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, EVENT_CAPACITY, "event storage was unbounded");
        Ok(())
    }
}
