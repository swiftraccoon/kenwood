# kenwood-transport

Model-neutral asynchronous byte transports for device libraries and protocol
clients. The crate owns byte I/O, not radio identity or operating-mode policy.

- `Transport`: exclusively owned asynchronous byte I/O with cancellation-safe
  reads; canceling a write may leave a transmitted prefix.
- `MockTransport`: exact FIFO writes, partial reads, hangs, and scripted
  reopen outcomes for hardware-free tests.
- `StreamAdapter`: a `Transport` exposed as Tokio `AsyncRead` and `AsyncWrite`,
  with a bounded outbound write channel, completion-aware flush, and typed
  recovery.
- `serial::SerialTransport`: explicit serial settings and resource ownership;
  available with the default `serial` feature.
- `bluetooth::BluetoothTransport`: opt-in macOS native RFCOMM through a
  bounded, isolated helper; portable validated selectors accompany it.

Device libraries choose endpoints, line presets, framing, retries, readiness
checks, and mode transitions. Opening a byte connection identifies neither the
attached device nor its protocol state. This crate never sends a
reset, mode-switch, or protocol-recovery command on its own.

Choose the surface that owns your task: `MockTransport` for protocol tests,
`StreamAdapter` for Tokio stream consumers, a model crate's wrapper for a radio,
or the explicit serial/native backends when your application owns endpoint
selection and lifecycle policy. All examples marked `rust` execute offline as
doctests. The serial/native opening examples are `no_run`: they compile but
must not be run without a deliberately selected endpoint.

## Completion and ownership

Transport success is a host-side observation, not command acceptance by a
device. The backend determines the precise write-completion boundary:

| Backend | Completion boundary of a successful write | Not covered by success |
| --- | --- | --- |
| Mock | One matching expected write was consumed and its response queued | Response consumption or physical I/O |
| Serial | All bytes and the serial backend's stream flush completed | Independent hardware drain or peer receipt |
| Native Bluetooth | All bytes entered helper stdin | RFCOMM write completion or peer receipt |

`StreamAdapter::flush` waits for those underlying calls; it does not strengthen
their guarantees. Protocol framing, response validation and readback belong to
the caller. Preserve operation failures separately from cleanup failures. An
I/O error or timeout does not mean zero bytes reached the peer, so resending the
same command can duplicate a partial transmission; re-establish the protocol
boundary before retrying.

## Script an exchange

Every expected write is checked in order. `assert_complete` checks only the
remaining expected-write queue. Check each write result and response explicitly;
unread bytes and a separate reopen script are not covered. Use
`assert_reopen_script_complete` when you script reopen outcomes.
`from_fixture` parses leniently: unrecognized lines and unmatched commands or
responses are ignored, so a fixture can load into an empty script, on which
`assert_complete` passes. Its rustdoc defines the complete grammar; check
`writes()` for the commands the fixture was meant to script.

```rust
use kenwood_transport::{MockTransport, Transport};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut transport = MockTransport::new();
    transport.expect(b"PING\r", b"PONG\r");

    transport.write(b"PING\r").await?;
    let mut response = [0_u8; 5];
    let count = transport.read(&mut response).await?;
    assert_eq!(count, response.len());
    assert_eq!(&response, b"PONG\r");
    transport.assert_complete();
    transport.close().await?;
    Ok(())
}
```

## Use a Tokio byte-stream consumer

`StreamAdapter::new` starts a pump on the current Tokio runtime. A successful
`write_all` admits bytes to its bounded queue; `flush` confirms that the
underlying transport completed every accepted write at the boundary above.
There is no additional delivery acknowledgement. A pending read does not
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
the failure's reconstructed `io::ErrorKind` and display text and, when available,
the owned transport. It does not preserve the original typed source/downcast
identity. The `StreamRecoveryError` rustdoc includes an executable EOF example
that retains the operation error while closing the recovered mock separately.
A pump-task panic cannot retain that ownership.

Dropping the adapter closes its channels but does not join its pump. An accepted
transport write can keep that task and its connection alive indefinitely.
`shutdown_and_recover` is consuming and not cancellation-safe: dropping its
pending future detaches the pump and loses the transport. It has no intrinsic
deadline. Retain and join the future, even after an observation timeout; its
rustdoc demonstrates passing `&mut recovery` to a timeout instead of discarding
the owned future. Recovery discards unread adapter buffers. Neither stream
shutdown nor recovery calls `Transport::close`; close the returned owner under
your backend's policy before claiming retirement.

## Serial feature

The default `serial` feature provides exclusive 8N1 serial I/O with explicit
`SerialOptions`: a nonzero baud rate, flow control, DTR/RTS policy, and close
behavior. It requires an active Tokio I/O runtime. It does not discover a
radio, select a model-specific preset, or automatically reopen a connection.
Read/write/shutdown have no intrinsic deadline. A canceled write can leave a
prefix; a canceled close still drops the descriptor because it was taken before
awaiting shutdown. Preserve any close failure, even though a second close is a
no-op success. Open and baud changes are synchronous host calls, not operations
an async timeout can preempt.

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
configuration. Address, name, and channel types are portable; `BluetoothTransport`
exists only on macOS. `BluetoothAddress` rustdoc contains an executable offline
normalization and validation example, available with this feature on every
platform. Validation sends no traffic and proves no pairing or reachability.
Each open selects one paired device and performs one bounded
attempt. Exact addresses never fall back to names, and ambiguous names fail.
`BluetoothService::SerialPort` requires a fresh successful SDP callback and
exactly one Serial Port service record. `FixedChannel` uses the caller's
explicit channel, such as a model-qualified value or the channel returned by
its previous successful opening of the same device; it does not discover one.

Native open, paired-device inventory and helper validation share one
process-global lease, even for different addresses. Discover or validate before
opening a connection. Overlap returns a `BluetoothHelper` error whose source
kind is `WouldBlock`; it does not queue or fall back. A returned connection holds
the lease until its helper is reaped. Deferred cleanup retains it in the reaper,
so dropping the transport does not necessarily make another open admissible.

Open and inventory are synchronous and use a 22-second parent readiness or
discovery deadline, with additional cleanup. Async applications must retain and
join their blocking worker. Cancellation is sticky for that opening/inventory
operation; canceling the token does not close an already returned transport or
join its worker. Close an unwanted late owner as well as the successful owner
you intend to use. Helper validation is a separate five-second no-radio
exchange and uses the same lease.

Before SDP, the helper gives pending startup events one default-mode run-loop
slice with a 50 ms timeout request, even when the selected device already
reports connected. That slice is drawn from the 20-second native opening budget
and may return early or overrun the request. The helper rechecks the deadline
afterward; the parent process still enforces cancellation and timeout. The slice
performs no service query, channel retry, or baseband reset.

Set `KENWOOD_BT_TRACE=1` in the parent process to enable the helper's shim
tracing. The helper inherits stderr, so each trace line carries the helper PID
and can be correlated with the Rust transport log without touching the raw
stdout byte stream.

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
let endpoint = (transport.address().clone(), transport.channel());
let close_result = transport.close().await;
drop(transport);
close_result?;
println!("{} channel {}", endpoint.0, endpoint.1.get());
# Ok(())
# }
```

All native objects remain in the killable helper. Native reads have no intrinsic
deadline. A failed or canceled pending write invalidates the helper rather than
retaining a possibly truncated command stream; pipe acceptance still proves
neither native completion nor delivery. The five-second write deadline is
checked on pipe backpressure, not a preemptive whole-call wall-clock limit.

Native `close` performs synchronous teardown in one poll despite its async
signature: a 600 ms graceful window plus, when needed, 100 ms of synchronous
reap checking. OS calls and scheduling are not a real-time guarantee. A Tokio
timeout cannot interrupt that poll, and another task on the same single-thread
executor cannot run during it. Drop can perform the same work. Close retains
its first outcome, including errors; repeated calls cannot turn an uncertain
cleanup into a confirmed close. Unconfirmed native closure, an unsuccessful
helper exit, forced termination and pending reaping all leave the channel
possibly open. Await close explicitly and retain its independent result;
Drop is best-effort and reports no outcome.

Native startup distinguishes an absent paired device (`TransportError::NotFound`)
from `TransportError::BluetoothOpen` with a typed `error::BluetoothOpenStage`.
Stages identify startup event-processing expiry, service-discovery dispatch,
completion, deadline or resolution, and RFCOMM dispatch, completion, deadline or
endpoint validation. A stage records what the host observed, not a cause in the
radio's firmware. When an opening stage coincides with an independent native
cleanup failure, `TransportError::BluetoothOpenWithCleanup` preserves both; it
is produced only from a complete private failure record, a matching helper exit
status and actual process reaping, and the channel may still be open in the OS.
Malformed or mismatched records are reported as helper failures. This crate
never retries after either error; retry policy belongs to the model wrapper.

The reported address and channel describe the successfully opened endpoint,
not a radio identity, protocol readiness, or operating mode. Native transport
never reopens a closed connection on its own. Sandboxed applications may
supply an absolute separately signed helper executable and validate its
no-radio echo lifecycle before use. Pairing and model-specific recovery remain
caller policy.
