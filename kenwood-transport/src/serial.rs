//! Exclusive asynchronous serial byte streams with explicit host line policy.
//!
//! Every endpoint uses eight data bits, no parity, and one stop bit (8N1).
//! Unix descriptors request exclusive access; Windows serial opens are
//! exclusive by the platform API. Callers supply every variable setting.
//! This module does not enumerate devices, recognize names, choose a radio,
//! retry opens, or infer that an open endpoint is ready for a protocol.
//!
//! Available on supported host platforms with the `serial` feature, enabled
//! by default. Start with [`SerialOptions`] and [`SerialTransport::open`];
//! see [`SerialTransport`] for completion, cancellation and close semantics.

use std::num::NonZeroU32;

use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_serial::{DataBits, Parity, SerialPort, SerialPortBuilder, SerialStream, StopBits};

use crate::{Transport, TransportError};

/// Host-side serial flow control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowControl {
    /// No flow control.
    None,
    /// RTS/CTS hardware flow control.
    Hardware,
    /// XON/XOFF software flow control.
    Software,
}

impl FlowControl {
    const fn native(self) -> tokio_serial::FlowControl {
        match self {
            Self::None => tokio_serial::FlowControl::None,
            Self::Hardware => tokio_serial::FlowControl::Hardware,
            Self::Software => tokio_serial::FlowControl::Software,
        }
    }
}

/// Whether opening an endpoint explicitly changes a modem-control line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineState {
    /// Do not request a line change. The operating system may still change
    /// the line during open; this is not a guarantee of electrical continuity.
    Preserve,
    /// Request the asserted state.
    Assert,
    /// Request the deasserted state.
    Deassert,
}

impl LineState {
    const fn requested(self) -> Option<bool> {
        match self {
            Self::Preserve => None,
            Self::Assert => Some(true),
            Self::Deassert => Some(false),
        }
    }
}

/// Descriptor-release policy selected by the endpoint owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseMode {
    /// Request asynchronous stream shutdown before dropping the descriptor.
    Shutdown,
    /// Release the descriptor without a stream shutdown request.
    Drop,
}

/// Complete variable configuration for an exclusive 8N1 serial endpoint.
///
/// There is deliberately no default radio preset. A model-specific caller
/// chooses the baud, flow control, modem lines, and close policy together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialOptions {
    /// Nonzero host line-coding baud rate.
    pub baud: NonZeroU32,
    /// Host flow control, independent of explicit DTR/RTS requests.
    pub flow_control: FlowControl,
    /// DTR policy applied during open and, when explicit, again afterward.
    pub dtr: LineState,
    /// RTS policy applied after open when explicit.
    pub rts: LineState,
    /// How this descriptor is released by [`Transport::close`].
    pub close_mode: CloseMode,
}

impl SerialOptions {
    fn builder(self, path: &str) -> SerialPortBuilder {
        let builder = tokio_serial::new(path, self.baud.get())
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(self.flow_control.native());
        let builder = match self.dtr.requested() {
            Some(state) => builder.dtr_on_open(state),
            None => builder.preserve_dtr_on_open(),
        };
        #[cfg(unix)]
        let builder = builder.exclusive(true);
        builder
    }
}

/// An exclusively owned asynchronous serial descriptor.
///
/// Available with the `serial` feature, enabled by default.
///
/// Writes complete only after all supplied bytes and the stream flush
/// complete according to the serial backend. This is not independent
/// hardware-drain or peer-receipt evidence; only errors surfaced by the
/// backend can be preserved. Canceling a write may leave a transmitted prefix;
/// no retry is performed. Reads expose received bytes without protocol
/// interpretation.
/// Closing takes ownership of the descriptor before awaiting shutdown, so
/// cancellation still drops it and leaves this transport closed.
/// Read, write and asynchronous shutdown have no intrinsic deadline. An
/// ordinary async timeout can cancel their pending futures, but does not
/// preempt the synchronous host work performed by open or a baud change.
///
/// A short read is normal. An open descriptor forwards zero-length reads and
/// EOF according to its stream backend; a closed descriptor returns
/// [`TransportError::Disconnected`] even for an empty buffer. An empty write
/// still performs the stream flush. Reads and writes after close starts fail;
/// a repeated close succeeds without replacing an earlier close error.
///
/// [`Transport::reopen`] is unsupported. Reopening and re-establishing protocol
/// identity are responsibilities of the model-specific owner.
#[derive(Debug)]
pub struct SerialTransport {
    /// `None` after close starts, including while shutdown is pending.
    port: Option<SerialStream>,
    /// Caller-selected path, retained for error context only.
    path: String,
    /// Last successfully applied host baud rate.
    baud: NonZeroU32,
    /// Caller-selected descriptor-release policy.
    close_mode: CloseMode,
}

impl SerialTransport {
    /// Open exactly `path` with the supplied exclusive 8N1 configuration.
    ///
    /// This synchronous host operation must run within an I/O-enabled Tokio
    /// runtime. It performs no radio handshake and has no intrinsic deadline.
    /// Any explicit modem-line error fails the open and releases the descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] when no Tokio runtime is active, the
    /// endpoint cannot be opened, or an explicit modem-line request fails.
    /// Operating-system error kinds and messages are preserved.
    pub fn open(path: &str, options: SerialOptions) -> Result<Self, TransportError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(open_error(
                path,
                std::io::Error::other("no tokio runtime is active on this thread"),
            ));
        }
        tracing::info!(path, ?options, "serial open requested");
        let mut port =
            SerialStream::open(&options.builder(path)).map_err(|error| open_error(path, error))?;
        apply_modem_lines(options, |line, state| match line {
            ModemLine::Dtr => port.write_data_terminal_ready(state),
            ModemLine::Rts => port.write_request_to_send(state),
        })
        .map_err(|error| open_error(path, error))?;
        tracing::info!(path, "serial open completed");
        Ok(Self {
            port: Some(port),
            path: path.to_owned(),
            baud: options.baud,
            close_mode: options.close_mode,
        })
    }

    fn port_mut(&mut self) -> Result<&mut SerialStream, TransportError> {
        self.port.as_mut().ok_or_else(closed_error)
    }
}

impl Transport for SerialTransport {
    /// Write all bytes, then flush the serial stream backend.
    ///
    /// Success is not independent hardware-drain or peer-receipt evidence.
    /// See the [type contract](Self) for cancellation and deadline behavior.
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        tracing::trace!(path = %self.path, raw = ?data, "serial write requested");
        write_flushed(self.port_mut()?, data).await?;
        tracing::debug!(path = %self.path, bytes = data.len(), "serial write completed");
        Ok(())
    }

    /// Read a stream prefix without an intrinsic deadline or protocol framing.
    ///
    /// This follows [`Transport::read`]'s cancellation contract. Backend read
    /// errors use [`TransportError::Read`]; closed access uses
    /// [`TransportError::Disconnected`].
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let count = self
            .port_mut()?
            .read(buffer)
            .await
            .map_err(TransportError::Read)?;
        tracing::debug!(path = %self.path, bytes = count, "serial read completed");
        if let Some(received) = buffer.get(..count) {
            tracing::trace!(path = %self.path, raw = ?received, "serial bytes received");
        }
        Ok(count)
    }

    /// Take the descriptor, optionally request shutdown, and then drop it.
    ///
    /// Pending-future cancellation still drops the taken descriptor. A
    /// shutdown error is [`TransportError::Disconnected`]; this owner remains
    /// closed even on that error. There is no intrinsic shutdown deadline.
    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::info!(path = %self.path, "serial close requested");
        close_port(&mut self.port, self.close_mode).await?;
        tracing::info!(path = %self.path, "serial close completed");
        Ok(())
    }

    /// Apply a nonzero baud rate without changing the endpoint identity.
    ///
    /// Closed access returns [`TransportError::Disconnected`]. Zero or an OS
    /// failure returns [`TransportError::Open`], retaining the last successful
    /// rate. This synchronous host call has no intrinsic deadline.
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        let path = self.path.clone();
        let previous_baud = self.baud;
        let port = self.port_mut()?;
        let baud = NonZeroU32::new(baud).ok_or_else(|| {
            open_error(
                &path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "serial baud rate must be nonzero",
                ),
            )
        })?;
        tracing::debug!(path, %previous_baud, %baud, "serial baud change requested");
        port.set_baud_rate(baud.get())
            .map_err(|error| open_error(&path, error))?;
        self.baud = baud;
        tracing::debug!(path, %baud, "serial baud change completed");
        Ok(())
    }
}

/// Explicit post-open modem-line operations, in DTR then RTS order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModemLine {
    Dtr,
    Rts,
}

fn apply_modem_lines<E>(
    options: SerialOptions,
    mut apply: impl FnMut(ModemLine, bool) -> Result<(), E>,
) -> Result<(), E> {
    for (line, state) in [(ModemLine::Dtr, options.dtr), (ModemLine::Rts, options.rts)] {
        if let Some(requested) = state.requested() {
            apply(line, requested)?;
        }
    }
    Ok(())
}

async fn write_flushed<P: AsyncWrite + Unpin>(
    port: &mut P,
    data: &[u8],
) -> Result<(), TransportError> {
    port.write_all(data).await.map_err(TransportError::Write)?;
    port.flush().await.map_err(TransportError::Write)
}

async fn close_port<P: AsyncWrite + Unpin>(
    slot: &mut Option<P>,
    mode: CloseMode,
) -> Result<(), TransportError> {
    let Some(mut port) = slot.take() else {
        return Ok(());
    };
    if mode == CloseMode::Shutdown {
        port.shutdown()
            .await
            .map_err(TransportError::Disconnected)?;
    }
    Ok(())
}

fn closed_error() -> TransportError {
    TransportError::Disconnected(std::io::Error::new(
        std::io::ErrorKind::NotConnected,
        "serial endpoint is closed",
    ))
}

fn open_error(path: &str, source: impl Into<std::io::Error>) -> TransportError {
    TransportError::Open {
        path: path.to_owned(),
        source: source.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::task::{Context, Poll};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const TEST_BAUD: NonZeroU32 = NonZeroU32::new(19_200).unwrap();

    const fn options() -> SerialOptions {
        SerialOptions {
            baud: TEST_BAUD,
            flow_control: FlowControl::None,
            dtr: LineState::Preserve,
            rts: LineState::Preserve,
            close_mode: CloseMode::Shutdown,
        }
    }

    #[test]
    fn builders_pin_exclusive_8n1_and_explicit_flow_and_dtr() {
        for flow_control in [
            FlowControl::None,
            FlowControl::Hardware,
            FlowControl::Software,
        ] {
            for dtr in [LineState::Preserve, LineState::Assert, LineState::Deassert] {
                let options = SerialOptions {
                    flow_control,
                    dtr,
                    ..options()
                };
                let expected_flow = match flow_control {
                    FlowControl::None => tokio_serial::FlowControl::None,
                    FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
                    FlowControl::Software => tokio_serial::FlowControl::Software,
                };
                let expected = tokio_serial::new("synthetic-endpoint", TEST_BAUD.get())
                    .data_bits(DataBits::Eight)
                    .parity(Parity::None)
                    .stop_bits(StopBits::One)
                    .flow_control(expected_flow);
                let expected = match dtr {
                    LineState::Preserve => expected.preserve_dtr_on_open(),
                    LineState::Assert => expected.dtr_on_open(true),
                    LineState::Deassert => expected.dtr_on_open(false),
                };
                #[cfg(unix)]
                let expected = expected.exclusive(true);
                assert_eq!(options.builder("synthetic-endpoint"), expected);
            }
        }
    }

    #[test]
    fn modem_line_policy_preserves_omitted_lines_and_orders_explicit_requests() -> TestResult {
        for dtr in [LineState::Preserve, LineState::Assert, LineState::Deassert] {
            for rts in [LineState::Preserve, LineState::Assert, LineState::Deassert] {
                let options = SerialOptions {
                    dtr,
                    rts,
                    ..options()
                };
                let mut actual = Vec::new();
                apply_modem_lines(options, |line, state| {
                    actual.push((line, state));
                    Ok::<(), std::io::Error>(())
                })?;
                let mut expected = Vec::new();
                for (line, state) in [(ModemLine::Dtr, dtr), (ModemLine::Rts, rts)] {
                    match state {
                        LineState::Preserve => {}
                        LineState::Assert => expected.push((line, true)),
                        LineState::Deassert => expected.push((line, false)),
                    }
                }
                assert_eq!(actual, expected);
            }
        }
        Ok(())
    }

    #[test]
    fn modem_line_failure_stops_configuration_immediately() {
        let options = SerialOptions {
            dtr: LineState::Assert,
            rts: LineState::Assert,
            ..options()
        };
        let mut calls = Vec::new();
        let result = apply_modem_lines(options, |line, state| {
            calls.push((line, state));
            Err(std::io::ErrorKind::PermissionDenied)
        });
        assert_eq!(result, Err(std::io::ErrorKind::PermissionDenied));
        assert_eq!(calls, [(ModemLine::Dtr, true)]);
    }

    #[test]
    fn serial_errors_preserve_kind_path_and_description() -> TestResult {
        for (serial_kind, expected_kind) in [
            (
                tokio_serial::ErrorKind::NoDevice,
                std::io::ErrorKind::NotFound,
            ),
            (
                tokio_serial::ErrorKind::Io(std::io::ErrorKind::PermissionDenied),
                std::io::ErrorKind::PermissionDenied,
            ),
            (
                tokio_serial::ErrorKind::InvalidInput,
                std::io::ErrorKind::InvalidInput,
            ),
        ] {
            let error = open_error(
                "synthetic-endpoint",
                tokio_serial::Error::new(serial_kind, "source description"),
            );
            let TransportError::Open { path, source } = error else {
                return Err(format!("expected an open error, got {error:?}").into());
            };
            assert_eq!(path, "synthetic-endpoint");
            assert_eq!(source.kind(), expected_kind);
            assert_eq!(source.to_string(), "source description");
        }
        Ok(())
    }

    #[test]
    fn missing_runtime_fails_before_endpoint_open() -> TestResult {
        let result = SerialTransport::open("synthetic-endpoint", options());
        let Err(TransportError::Open { path, source }) = result else {
            return Err(format!("expected an open error, got {result:?}").into());
        };
        assert_eq!(path, "synthetic-endpoint");
        assert!(source.to_string().contains("tokio runtime"));
        Ok(())
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Write(Vec<u8>),
        Flush,
        Shutdown,
        Drop,
    }

    #[derive(Clone, Copy)]
    enum Completion {
        Success,
        Failed,
        Pending,
    }

    impl Completion {
        fn poll(self) -> Poll<std::io::Result<()>> {
            match self {
                Self::Success => Poll::Ready(Ok(())),
                Self::Failed => Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "scripted failure",
                ))),
                Self::Pending => Poll::Pending,
            }
        }
    }

    struct Writer {
        events: Rc<RefCell<Vec<Event>>>,
        write: Completion,
        flush: Completion,
        shutdown: Completion,
    }

    impl Writer {
        fn new(events: &Rc<RefCell<Vec<Event>>>) -> Self {
            Self {
                events: Rc::clone(events),
                write: Completion::Success,
                flush: Completion::Success,
                shutdown: Completion::Success,
            }
        }
    }

    impl AsyncWrite for Writer {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let count = buffer.len().min(2);
            let Some(bytes) = buffer.get(..count) else {
                return Poll::Ready(Err(std::io::Error::other("invalid test write range")));
            };
            self.events.borrow_mut().push(Event::Write(bytes.to_vec()));
            self.write.poll().map(|result| result.map(|()| count))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.events.borrow_mut().push(Event::Flush);
            self.flush.poll()
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.events.borrow_mut().push(Event::Shutdown);
            self.shutdown.poll()
        }
    }

    impl Drop for Writer {
        fn drop(&mut self) {
            self.events.borrow_mut().push(Event::Drop);
        }
    }

    #[tokio::test]
    async fn writes_complete_only_after_all_partial_writes_and_flush() -> TestResult {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut writer = Writer::new(&events);
        write_flushed(&mut writer, b"abcde").await?;
        assert_eq!(
            *events.borrow(),
            [
                Event::Write(b"ab".to_vec()),
                Event::Write(b"cd".to_vec()),
                Event::Write(b"e".to_vec()),
                Event::Flush
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn flush_failure_retains_the_write_error_kind() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut writer = Writer::new(&events);
        writer.flush = Completion::Failed;
        let result = write_flushed(&mut writer, b"ab").await;
        assert!(
            matches!(result, Err(TransportError::Write(error)) if error.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(
            *events.borrow(),
            [Event::Write(b"ab".to_vec()), Event::Flush]
        );
    }

    #[tokio::test]
    async fn write_failure_retains_its_kind_and_never_flushes() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut writer = Writer::new(&events);
        writer.write = Completion::Failed;
        let result = write_flushed(&mut writer, b"ab").await;
        assert!(
            matches!(result, Err(TransportError::Write(error)) if error.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(*events.borrow(), [Event::Write(b"ab".to_vec())]);
    }

    #[tokio::test]
    async fn canceling_a_pending_flush_never_completes_or_retries_a_write() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut writer = Writer::new(&events);
        writer.flush = Completion::Pending;
        let mut pending = Box::pin(write_flushed(&mut writer, b"ab"));
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(pending);
        assert_eq!(
            *events.borrow(),
            [Event::Write(b"ab".to_vec()), Event::Flush]
        );
    }

    #[tokio::test]
    async fn close_modes_release_once_and_shutdown_failures_leave_no_owner() -> TestResult {
        for mode in [CloseMode::Shutdown, CloseMode::Drop] {
            let events = Rc::new(RefCell::new(Vec::new()));
            let mut slot = Some(Writer::new(&events));
            close_port(&mut slot, mode).await?;
            close_port(&mut slot, mode).await?;
            assert!(slot.is_none());
            let expected = if mode == CloseMode::Shutdown {
                vec![Event::Shutdown, Event::Drop]
            } else {
                vec![Event::Drop]
            };
            assert_eq!(*events.borrow(), expected);
        }
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut writer = Writer::new(&events);
        writer.shutdown = Completion::Failed;
        let mut slot = Some(writer);
        let result = close_port(&mut slot, CloseMode::Shutdown).await;
        assert!(
            matches!(result, Err(TransportError::Disconnected(error)) if error.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert!(slot.is_none());
        assert_eq!(*events.borrow(), [Event::Shutdown, Event::Drop]);
        Ok(())
    }

    #[tokio::test]
    async fn canceling_shutdown_releases_the_descriptor_and_leaves_closed_state() -> TestResult {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut writer = Writer::new(&events);
        writer.shutdown = Completion::Pending;
        let mut slot = Some(writer);
        let mut pending = Box::pin(close_port(&mut slot, CloseMode::Shutdown));
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(pending);
        assert!(slot.is_none());
        assert_eq!(*events.borrow(), [Event::Shutdown, Event::Drop]);
        close_port(&mut slot, CloseMode::Shutdown).await?;
        assert_eq!(*events.borrow(), [Event::Shutdown, Event::Drop]);
        Ok(())
    }

    #[tokio::test]
    async fn closed_endpoints_fail_consistently_without_reopen() -> TestResult {
        let mut transport = SerialTransport {
            port: None,
            path: "synthetic-endpoint".to_owned(),
            baud: TEST_BAUD,
            close_mode: CloseMode::Shutdown,
        };
        let mut buffer = [0; 8];
        for result in [
            transport.write(b"abc").await,
            transport.read(&mut buffer).await.map(|_| ()),
            transport.set_baud_rate(9_600),
        ] {
            assert!(
                matches!(result, Err(TransportError::Disconnected(error)) if error.kind() == std::io::ErrorKind::NotConnected)
            );
        }
        transport.close().await?;
        assert!(matches!(
            transport.reopen().await,
            Err(TransportError::ReopenUnsupported)
        ));
        assert!(transport.port.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pseudo_terminal_exchanges_bytes_and_rejects_zero_baud() -> TestResult {
        let (port, mut peer) = SerialStream::pair()?;
        let mut transport = SerialTransport {
            port: Some(port),
            path: "owned-test-pty".to_owned(),
            baud: TEST_BAUD,
            close_mode: CloseMode::Shutdown,
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            transport.write(b"abc").await?;
            let mut received = [0; 3];
            let received_count = peer.read_exact(&mut received).await?;
            assert_eq!(received_count, received.len());
            assert_eq!(received, *b"abc");
            assert!(tokio::time::timeout(
                std::time::Duration::from_millis(1),
                transport.read(&mut received),
            ).await.is_err(), "PTY read completed without a peer reply");
            peer.write_all(b"xyz").await?;
            let mut total = 0;
            while total < received.len() {
                let buffer = received.get_mut(total..).ok_or("invalid receive range")?;
                let count = transport.read(buffer).await?;
                assert_ne!(count, 0, "PTY closed before reply completed");
                total += count;
            }
            assert_eq!(received, *b"xyz");
            let result = transport.set_baud_rate(0);
            assert!(matches!(result, Err(TransportError::Open { path, source }) if path == "owned-test-pty" && source.kind() == std::io::ErrorKind::InvalidInput));
            assert_eq!(transport.baud, TEST_BAUD);
            transport.close().await?;
            assert!(transport.port.is_none());
            Ok::<(), Box<dyn std::error::Error>>(())
        }).await??;
        Ok(())
    }
}
