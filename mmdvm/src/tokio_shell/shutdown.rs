//! Completed modem shutdown, retained events, and exact queued-frame loss.

use std::fmt;

use crate::ShellError;

use super::Event;

/// Bounded observations retained when the modem task has stopped.
///
/// The event slice contains at most the event ring's 256 newest unread events.
/// The dropped-frame count comes directly from the completed task and remains
/// available even if its [`Event::TxDropped`] was consumed earlier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownSummary {
    pub(crate) dropped_frames: Option<usize>,
    pub(crate) events: Vec<Event>,
    pub(crate) skipped_events: u64,
}

impl ShutdownSummary {
    /// Frames still queued when the task stopped, or `None` after task interruption.
    ///
    /// A frame removed for a failed write can have reached the device partially;
    /// it is covered by the I/O error rather than this remaining-queue count.
    #[must_use]
    pub const fn dropped_frames(&self) -> Option<usize> {
        self.dropped_frames
    }

    /// Retained unread events in their original delivery order.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Unread events overwritten before the retained event slice.
    #[must_use]
    pub const fn skipped_events(&self) -> u64 {
        self.skipped_events
    }
}

/// Recovered stream and observations after a clean modem task exit.
pub struct ModemShutdown<T> {
    pub(crate) transport: T,
    pub(crate) summary: ShutdownSummary,
}

impl<T> ModemShutdown<T> {
    /// Queued-frame loss and retained events from the completed task.
    #[must_use]
    pub const fn summary(&self) -> &ShutdownSummary {
        &self.summary
    }

    /// Recover the stream after inspecting the shutdown summary.
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Transfer the recovered stream and its observations without cloning events.
    #[must_use]
    pub fn into_parts(self) -> (T, ShutdownSummary) {
        (self.transport, self.summary)
    }
}

impl<T> fmt::Debug for ModemShutdown<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModemShutdown")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

/// Failed modem task with retained observations; its stream has been dropped.
#[derive(Debug, thiserror::Error)]
#[error("MMDVM shutdown failed: {error}")]
pub struct ShutdownFailure {
    #[source]
    pub(crate) error: ShellError,
    pub(crate) summary: ShutdownSummary,
}

impl ShutdownFailure {
    /// Original typed task, codec, or transport failure.
    #[must_use]
    pub const fn error(&self) -> &ShellError {
        &self.error
    }

    /// Queued-frame loss and retained events, including failure diagnostics.
    #[must_use]
    pub const fn summary(&self) -> &ShutdownSummary {
        &self.summary
    }
}

/// Task result before unread broadcast events are collected by the handle.
pub(crate) struct TaskCompletion<T> {
    pub(crate) transport: Result<T, ShellError>,
    pub(crate) dropped_frames: usize,
}
