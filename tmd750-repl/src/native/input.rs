//! Owned command input without an uncancellable background stdin reader.
//!
//! Rich terminals retain Rustyline editing and history on a joined worker.
//! Rustyline's Unix signal handlers are process-global: this adapter assumes
//! exclusive terminal ownership. A retained Tokio SIGINT registration covers
//! Rustyline's handler-installation and retirement windows. Once cancellation
//! is requested, thread-directed SIGINT wakes only the owned reader. The thread
//! stays joinable until completion is observed and its owner joins it.
//! Pipes and plain terminals use nonblocking readiness on Unix; cancellation
//! never needs their writer to close or supply a newline. Descriptor flags are
//! restored explicitly before this adapter is retired. Regular-file scripts
//! are bounded and loaded before the caller opens a radio connection.
//! Source mode remains explicit so redirected batches cannot inherit successful
//! interactive interruption or typo-recovery semantics.

use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;

use crate::{AppResult, PROMPT, output};

#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::thread::JoinHandleExt;
#[cfg(unix)]
use std::thread::JoinHandle;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
#[cfg(unix)]
use tokio::io::Interest;
#[cfg(unix)]
use tokio::io::unix::AsyncFd;

/// Maximum command bytes before the newline, including an optional CR.
const MAX_LINE_BYTES: usize = 4096;
#[cfg(unix)]
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
#[cfg(unix)]
const READ_BYTES: usize = 1024;
#[cfg(unix)]
const CANCELLATION_POLL: Duration = Duration::from_millis(10);

/// A complete command or an orderly end of input.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    Line(String),
    Eof,
    Interrupted,
}

/// Terminal sessions recover from typos; redirected commands form a batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InputMode {
    Interactive,
    Batch,
}

impl InputMode {
    const fn from_terminal(is_terminal: bool) -> Self {
        if is_terminal {
            Self::Interactive
        } else {
            Self::Batch
        }
    }
}

/// Input ownership remains separate from the radio and its protocol state.
pub(super) trait CommandInput {
    /// Keep completion policy tied to the original source, including after close.
    fn mode(&self) -> InputMode;

    /// Await one command, retaining any started terminal worker until it ends.
    async fn next(&mut self, cancelled: &AtomicBool) -> AppResult<Event>;

    /// Retire input and report an independent descriptor-restoration failure.
    fn close(&mut self) -> AppResult<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct SystemInput {
    source: Option<Source>,
    mode: InputMode,
}

#[derive(Debug)]
enum Source {
    Terminal(Box<TerminalInput>),
    #[cfg(unix)]
    Ready(Box<ReadyInput>),
    #[cfg(unix)]
    Script(ScriptInput),
}

impl SystemInput {
    /// Prepare input inside the I/O runtime, before opening a radio connection.
    pub(super) fn new() -> AppResult<Self> {
        let stdin = io::stdin();
        let mode = InputMode::from_terminal(stdin.is_terminal());
        let term = std::env::var("TERM").ok();
        let source = if rich_terminal(
            stdin.is_terminal(),
            io::stdout().is_terminal(),
            term.as_deref(),
        ) {
            Source::Terminal(Box::new(TerminalInput::new()?))
        } else {
            #[cfg(unix)]
            {
                let file = File::from(stdin.as_fd().try_clone_to_owned()?);
                if file.metadata()?.is_file() {
                    Source::Script(ScriptInput::load(file)?)
                } else {
                    Source::Ready(Box::new(ReadyInput::new(file)?))
                }
            }
            #[cfg(not(unix))]
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "cancellable native Bluetooth command input requires Unix or a supported terminal",
                )
                .into());
            }
        };
        Ok(Self {
            source: Some(source),
            mode,
        })
    }
}

impl CommandInput for SystemInput {
    fn mode(&self) -> InputMode {
        self.mode
    }

    async fn next(&mut self, cancelled: &AtomicBool) -> AppResult<Event> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(Event::Interrupted);
        }
        match self.source.as_mut().ok_or_else(input_closed)? {
            Source::Terminal(input) => input.next(cancelled).await,
            #[cfg(unix)]
            Source::Ready(input) => input.next(cancelled).await.map_err(Into::into),
            #[cfg(unix)]
            Source::Script(input) => input.next().map_err(Into::into),
        }
    }

    fn close(&mut self) -> AppResult<()> {
        let Some(source) = self.source.take() else {
            return Ok(());
        };
        #[cfg(unix)]
        if let Source::Ready(mut input) = source {
            input.close()?;
        }
        #[cfg(not(unix))]
        drop(source);
        Ok(())
    }
}

fn rich_terminal(input_is_terminal: bool, output_is_terminal: bool, term: Option<&str>) -> bool {
    // Rustyline installs its Unix interrupt wake pipe only for a TTY output.
    input_is_terminal
        && output_is_terminal
        && term.is_none_or(|term| {
            !["dumb", "cons25", "emacs"]
                .iter()
                .any(|unsupported| term.eq_ignore_ascii_case(unsupported))
        })
}

fn input_closed() -> io::Error {
    io::Error::other("command input is already closed")
}

fn line_too_long() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("command input exceeds {MAX_LINE_BYTES} bytes before the newline"),
    )
}

#[derive(Debug)]
struct TerminalInput {
    editor: Option<DefaultEditor>,
    #[cfg(unix)]
    _interrupts: tokio::signal::unix::Signal,
}

impl TerminalInput {
    fn new() -> AppResult<Self> {
        Ok(Self {
            editor: Some(DefaultEditor::new()?),
            #[cfg(unix)]
            _interrupts: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        })
    }

    async fn next(&mut self, cancelled: &AtomicBool) -> AppResult<Event> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(Event::Interrupted);
        }
        let editor = self.editor.take().ok_or_else(input_closed)?;
        #[cfg(unix)]
        let worker = std::thread::Builder::new()
            .name("tmd750-input".to_owned())
            .spawn(move || read_terminal(editor))?;
        #[cfg(unix)]
        let ((editor, result), wake_error) =
            join_terminal(worker, cancelled, interrupt_terminal).await?;
        #[cfg(not(unix))]
        let (editor, result) = tokio::task::spawn_blocking(move || read_terminal(editor)).await?;
        self.editor = Some(editor);
        #[cfg(unix)]
        if let Some(source) = wake_error {
            return Err(TerminalWakeFailure {
                source,
                reader_error: result.err(),
            }
            .into());
        }
        let event = terminal_event(result)?;
        if cancelled.load(Ordering::Relaxed) {
            return Ok(Event::Interrupted);
        }
        if let Event::Line(line) = &event
            && !line.trim().is_empty()
            && let Some(editor) = &mut self.editor
            && let Err(error) = editor.add_history_entry(line)
        {
            output::error(format_args!(
                "Warning: history entry was not recorded: {error}"
            ));
        }
        Ok(event)
    }
}

fn read_terminal(mut editor: DefaultEditor) -> (DefaultEditor, Result<String, ReadlineError>) {
    let result = editor.readline(PROMPT);
    (editor, result)
}

/// POSIX keeps a thread ID valid until termination and loss of joinability.
/// Borrow the still-owned handle, never cache its ID or signal after joining.
///
/// <https://pubs.opengroup.org/onlinepubs/9799919799/functions/V2_chap02.html#tag_16_09_02>
#[cfg(unix)]
fn interrupt_terminal<T>(worker: &JoinHandle<T>) -> io::Result<()> {
    // Nix 0.31 decodes pthread_kill using a -1/errno convention, so positive
    // POSIX error returns, including ESRCH, may not be exposed. Retain errors
    // the wrapper does expose, but never infer delivery or cancellation from
    // Ok(()). Only is_finished() followed by a successful join proves normal
    // worker completion. No signal attempt grants protocol or cleanup authority.
    nix::sys::pthread::pthread_kill(worker.as_pthread_t(), nix::sys::signal::Signal::SIGINT)
        .map_err(Into::into)
}

#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
#[error("terminal reader worker failed: {source}; cancellation wake failure: {wake_error:?}")]
struct TerminalJoinFailure {
    source: io::Error,
    wake_error: Option<io::Error>,
}

#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
#[error("waking terminal input failed: {source}; reader failure: {reader_error:?}")]
struct TerminalWakeFailure {
    source: io::Error,
    reader_error: Option<ReadlineError>,
}

/// Do not abandon a started reader, even if an independent wake attempt fails.
#[cfg(unix)]
async fn join_terminal<T>(
    worker: JoinHandle<T>,
    cancelled: &AtomicBool,
    mut interrupt: impl FnMut(&JoinHandle<T>) -> io::Result<()>,
) -> Result<(T, Option<io::Error>), TerminalJoinFailure> {
    let mut wake_error = None;
    loop {
        // is_finished() is the completion proof. A successful signal is not.
        if worker.is_finished() {
            return match worker.join() {
                Ok(value) => Ok((value, wake_error)),
                Err(panic) => Err(TerminalJoinFailure {
                    source: terminal_panic(panic.as_ref()),
                    wake_error,
                }),
            };
        }
        if cancelled.load(Ordering::Relaxed)
            && let Err(error) = interrupt(&worker)
            && wake_error.is_none()
        {
            wake_error = Some(error);
        }
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

#[cfg(unix)]
fn terminal_panic(panic: &(dyn std::any::Any + Send)) -> io::Error {
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    io::Error::other(format!("terminal reader panicked: {message}"))
}

fn terminal_event(result: Result<String, ReadlineError>) -> AppResult<Event> {
    match result {
        Ok(line) if line.len() > MAX_LINE_BYTES => Err(line_too_long().into()),
        Ok(line) => Ok(Event::Line(line)),
        Err(ReadlineError::Eof) => Ok(Event::Eof),
        Err(ReadlineError::Interrupted) => Ok(Event::Interrupted),
        #[cfg(unix)]
        Err(ReadlineError::Signal(rustyline::error::Signal::Interrupt)) => Ok(Event::Interrupted),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
#[derive(Debug, Default)]
struct LineBuffer {
    pending: VecDeque<u8>,
    partial: Vec<u8>,
    eof: bool,
}

#[cfg(unix)]
impl LineBuffer {
    fn next(&mut self) -> io::Result<Option<Event>> {
        while let Some(byte) = self.pending.pop_front() {
            if byte == b'\n' {
                if self.partial.last() == Some(&b'\r') {
                    let _cr = self.partial.pop();
                }
                return self.line().map(Some);
            }
            if self.partial.len() == MAX_LINE_BYTES {
                return Err(line_too_long());
            }
            self.partial.push(byte);
        }
        if self.eof {
            if self.partial.is_empty() {
                Ok(Some(Event::Eof))
            } else {
                self.line().map(Some)
            }
        } else {
            Ok(None)
        }
    }

    fn line(&mut self) -> io::Result<Event> {
        let bytes = std::mem::take(&mut self.partial);
        String::from_utf8(bytes)
            .map(Event::Line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn receive(&mut self, bytes: &[u8]) {
        self.eof = bytes.is_empty();
        self.pending.extend(bytes.iter().copied());
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ScriptInput {
    bytes: io::Cursor<Vec<u8>>,
    lines: LineBuffer,
}

#[cfg(unix)]
impl ScriptInput {
    fn load(file: File) -> io::Result<Self> {
        let mut bytes = Vec::new();
        let limit = u64::try_from(MAX_SCRIPT_BYTES + 1)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let _read = file.take(limit).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_SCRIPT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("command script exceeds {MAX_SCRIPT_BYTES} bytes"),
            ));
        }
        Ok(Self {
            bytes: io::Cursor::new(bytes),
            lines: LineBuffer::default(),
        })
    }

    fn next(&mut self) -> io::Result<Event> {
        loop {
            if let Some(event) = self.lines.next()? {
                return Ok(event);
            }
            let mut bytes = [0; READ_BYTES];
            let count = self.bytes.read(&mut bytes)?;
            self.lines.receive(bytes.get(..count).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid script read length")
            })?);
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct FlaggedFile {
    file: File,
    original_flags: OFlags,
    restored: bool,
}

#[cfg(unix)]
impl FlaggedFile {
    fn new(file: File) -> io::Result<Self> {
        let original_flags = fcntl_getfl(&file)?;
        fcntl_setfl(&file, original_flags | OFlags::NONBLOCK)?;
        Ok(Self {
            file,
            original_flags,
            restored: false,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        self.restore_with(|file, flags| fcntl_setfl(file, flags).map_err(Into::into))
    }

    fn restore_with(
        &mut self,
        restore: impl FnOnce(&File, OFlags) -> io::Result<()>,
    ) -> io::Result<()> {
        if !self.restored {
            restore(&self.file, self.original_flags)?;
            self.restored = true;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl AsRawFd for FlaggedFile {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

#[cfg(unix)]
impl Drop for FlaggedFile {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            output::error(format_args!(
                "Restoring command-input flags failed: {error}"
            ));
        }
    }
}

#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
#[error(
    "command input registration failed: {source}; restoring its flags also failed: {restoration}"
)]
struct RegistrationFailure {
    source: io::Error,
    restoration: io::Error,
}

#[cfg(unix)]
#[derive(Debug)]
struct ReadyInput {
    descriptor: Option<AsyncFd<FlaggedFile>>,
    lines: LineBuffer,
    terminal: bool,
}

#[cfg(unix)]
impl ReadyInput {
    fn new(file: File) -> AppResult<Self> {
        // Refuse missing runtime context before changing the descriptor flags.
        let _runtime = tokio::runtime::Handle::try_current()?;
        let terminal = file.is_terminal();
        let file = FlaggedFile::new(file)?;
        let descriptor = match AsyncFd::try_with_interest(file, Interest::READABLE) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                let (mut file, source) = error.into_parts();
                return match file.restore() {
                    Ok(()) => Err(source.into()),
                    Err(restoration) => Err(RegistrationFailure {
                        source,
                        restoration,
                    }
                    .into()),
                };
            }
        };
        Ok(Self {
            descriptor: Some(descriptor),
            lines: LineBuffer::default(),
            terminal,
        })
    }

    async fn next(&mut self, cancelled: &AtomicBool) -> io::Result<Event> {
        if self.terminal && !cancelled.load(Ordering::Relaxed) {
            let mut output = io::stdout().lock();
            output.write_all(PROMPT.as_bytes())?;
            output.flush()?;
        }
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Ok(Event::Interrupted);
            }
            if let Some(event) = self.lines.next()? {
                return Ok(event);
            }
            let descriptor = self.descriptor.as_ref().ok_or_else(input_closed)?;
            let mut ready = tokio::select! {
                biased;
                () = cancelled_input(cancelled) => return Ok(Event::Interrupted),
                ready = descriptor.readable() => ready?,
            };
            let mut bytes = [0; READ_BYTES];
            match ready.try_io(|descriptor| {
                let mut reader = &descriptor.get_ref().file;
                reader.read(&mut bytes)
            }) {
                Ok(Ok(count)) => self.lines.receive(bytes.get(..count).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid command-input read length",
                    )
                })?),
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(error)) => return Err(error),
                Err(_not_ready) => {}
            }
        }
    }

    fn close(&mut self) -> io::Result<()> {
        if let Some(descriptor) = self.descriptor.take() {
            descriptor.into_inner().restore()?;
        }
        Ok(())
    }
}

#[cfg(unix)]
async fn cancelled_input(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCELLATION_POLL).await;
    }
}

#[cfg(test)]
mod tests;
