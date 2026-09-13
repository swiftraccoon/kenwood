# kenwood-transport

Model-neutral asynchronous byte transports for device libraries and protocol
clients. The crate owns byte I/O, not radio identity or operating-mode policy.

- `Transport`: exclusively owned asynchronous byte I/O with cancellation-safe
  reads; canceling a write may leave a transmitted prefix.
- `MockTransport`: strict FIFO exchanges, partial reads, timeouts, and scripted
  reopen outcomes for hardware-free tests.
- `StreamAdapter`: a `Transport` exposed as Tokio `AsyncRead` and `AsyncWrite`,
  with bounded write admission, completion-aware flush, and typed recovery.
- `serial::SerialTransport`: explicit serial settings and resource ownership;
  available with the default `serial` feature.
- `bluetooth::BluetoothTransport`: opt-in macOS native RFCOMM through a
  bounded, isolated helper; portable validated selectors accompany it.

Device libraries choose endpoints, line presets, framing, retries, readiness
checks, and mode transitions. Opening a byte connection does not prove which
device is attached or whether its protocol is ready. This crate never sends a
reset, mode-switch, or protocol-recovery command on its own.

## Script an exchange

Every expected write is checked in order. `assert_complete` verifies that the
caller consumed the entire script.

```rust
use kenwood_transport::{MockTransport, Transport};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut transport = MockTransport::new();
transport.expect(b"PING\r", b"PONG\r");

transport.write(b"PING\r").await?;
let mut response = [0_u8; 5];
let count = transport.read(&mut response).await?;
assert_eq!(count, response.len());
assert_eq!(&response, b"PONG\r");
transport.assert_complete();
# Ok(())
# }
```

## Use a Tokio byte-stream consumer

`StreamAdapter::new` starts a pump on the current Tokio runtime. A successful
`write_all` admits bytes to its bounded queue; `flush` confirms that the
underlying transport completed every accepted write. A pending read does not
block an outgoing write. Transports must honor the cancellation contract on
`Transport::read` because outbound work may cancel an in-progress read.

```rust
use kenwood_transport::{MockTransport, StreamAdapter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut transport = MockTransport::new();
    transport.expect(b"PING\r", b"PONG\r");
    transport.pend_when_empty();
    let mut stream = StreamAdapter::new(transport);

    stream.write_all(b"PING\r").await?;
    stream.flush().await?;
    let mut response = [0_u8; 5];
    let _count = stream.read_exact(&mut response).await?;
    assert_eq!(&response, b"PONG\r");

    let transport = stream.shutdown_and_recover().await?;
    transport.assert_complete();
    Ok(())
}
```

Recovering the transport does not close or reopen it, and it does not restore
any device mode. If the pump fails, `StreamRecoveryError::into_parts` returns
the original I/O failure and, when available, the owned transport. A pump-task
panic cannot retain that ownership. Dropping the adapter releases its channels
but cannot wait for pending transport I/O; use `shutdown_and_recover` when
ownership and completion must be proved.

## Serial feature

The default `serial` feature provides exclusive 8N1 serial I/O with explicit
`SerialOptions`: a nonzero baud rate, flow control, DTR/RTS policy, and close
behavior. It requires an active Tokio I/O runtime. It does not discover a
radio, select a model-specific preset, or automatically reopen a connection.

Ordinary radio clients should use their model crate's serial wrapper. Code
that owns a different endpoint can specify its host settings directly:

```rust,no_run
# #[cfg(feature = "serial")]
# async fn example(path: &str) -> Result<(), Box<dyn std::error::Error>> {
use std::num::NonZeroU32;
use kenwood_transport::serial::{CloseMode, FlowControl, LineState, SerialOptions};
use kenwood_transport::{SerialTransport, Transport};

let options = SerialOptions {
    baud: NonZeroU32::new(57_600).ok_or("baud must be nonzero")?,
    flow_control: FlowControl::None,
    dtr: LineState::Preserve,
    rts: LineState::Preserve,
    close_mode: CloseMode::Shutdown,
};
let mut transport = SerialTransport::open(path, options)?;
// Opening proves neither device identity nor protocol readiness.
transport.close().await?;
# Ok(())
# }
```

Use `default-features = false` when only the trait, mock, or stream adapter is
needed. No feature configuration depends on a radio model or protocol crate.

## Native Bluetooth feature

Enable `native-bluetooth` explicitly. It does not change the default serial
configuration. Address, name, and channel types are portable; native opening
is macOS-only. Each open selects one paired device and performs one bounded
attempt. Exact addresses never fall back to names, and ambiguous names fail.
`BluetoothService::SerialPort` requires a fresh successful SDP callback and
exactly one Serial Port service record. `FixedChannel` uses the caller's
explicit channel, such as a model-qualified value or the channel returned by
its previous successful opening of the same device; it does not discover one.

Before SDP, the helper offers pending startup events one default-mode run-loop
slice, even when the selected device already reports connected. Its 50 ms
timeout request shares the existing 20-second native opening budget. This is
a scheduling mitigation, not a framework-readiness guarantee: the run loop may
finish early, and callbacks can overrun the requested interval. The helper
rechecks the deadline afterward; the parent process still enforces cancellation
and timeout. This adds no service query, channel retry, or baseband reset.

```rust,no_run
# #[cfg(all(feature = "native-bluetooth", target_os = "macos"))]
# async fn example() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
use kenwood_transport::bluetooth::{
    BluetoothDeviceSelector, BluetoothOpenCancellation, BluetoothService,
    BluetoothTransport,
};
use kenwood_transport::Transport;

let selector = BluetoothDeviceSelector::Address("00-11-22-33-44-55".parse()?);
let cancellation = BluetoothOpenCancellation::default();
let open_signal = cancellation.clone();
let task = tokio::task::spawn_blocking(move || {
    BluetoothTransport::open(&selector, BluetoothService::SerialPort, &open_signal)
});
// Retain cancellation and join the task even when the caller is interrupted.
let mut transport = task.await??;
println!("{} channel {}", transport.address(), transport.channel().get());
transport.close().await?;
# Ok(())
# }
```

All native objects remain in the killable helper. The parent bounds startup
to 22 seconds and teardown to a 600 ms graceful window plus a 100 ms reap
window. A failed write cancels that helper rather than retaining a possibly
truncated command stream. Deferred cleanup retains exclusive ownership until
the child is reaped. A close error distinguishes unconfirmed native closure,
unsuccessful helper exit, forced termination, and pending reaping; none is a
clean-release proof. Dropping is best-effort, so explicitly await `close` and
retain its result when evidence matters.

Native startup distinguishes an absent paired device (`TransportError::NotFound`)
from `TransportError::BluetoothOpen` with a typed `error::BluetoothOpenStage`.
Stages identify startup event-processing expiry, service-discovery dispatch,
completion, deadline or resolution,
and RFCOMM dispatch, completion, deadline or endpoint validation. They describe
the host's observation, not the cause in the radio's firmware or evidence that
a retry would succeed. When an opening stage also has an independent native
cleanup failure, `TransportError::BluetoothOpenWithCleanup` preserves both.
This requires a complete private failure record, a matching helper exit status
and actual process reaping. It does not prove cancellation of an OS operation.
Malformed or mismatched records remain helper failures; neither failure text nor
unconfirmed cleanup alone grants retry authority.

The reported address and channel describe the successfully opened endpoint,
not a radio identity, protocol readiness, or operating mode. Native transport
does not automatically retry or reopen. Sandboxed applications may supply an
absolute separately signed helper executable and validate its no-radio echo
lifecycle before use. Pairing and model-specific recovery remain caller policy.
