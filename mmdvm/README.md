# mmdvm

Async MMDVM modem I/O and a radio-neutral D-STAR voice runtime. Callers supply
an open connection; this crate never selects an endpoint, sends CAT commands,
changes radio routing, or reopens a connection.

## Choose the layer

| Feature selection | Entry point | Responsibility |
| --- | --- | --- |
| Default (`runtime`) | `AsyncModem::spawn` | Spawned modem loop, raw events, status polling, and bounded TX queues |
| `default-features = false`, `features = ["probe"]` | `probe::probe_version`, `probe::probe_diagnostics` | Bounded version or version/status queries; no spawned modem loop |
| `features = ["dstar"]` | `dstar::DstarModem::initialize` | D-STAR initialization, voice events, slow data, last-heard state, and echo playback; enables `runtime` |

Enable both `probe` and `dstar` when one application needs identification and
voice operation. With all features disabled, only the shared shell error and
async-stream trait remain alongside the core re-export.

The production workspace dependency graph has no radio-model crate:

| Dependency | Enabled by | Owns |
| --- | --- | --- |
| [`mmdvm-core`](https://github.com/swiftraccoon/kenwood/tree/main/mmdvm-core) | Always | Sans-I/O MMDVM framing and modem response types |
| [`dstar-gateway-core`](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway-core) | `dstar` | D-STAR headers, voice, slow data, and URCALL classification |
| [`kenwood-transport`](https://github.com/swiftraccoon/kenwood/tree/main/kenwood-transport) | `probe` | Byte-transport trait and errors; its serial feature is not enabled here |

Reflector networking belongs to the separate
[`dstar-gateway`](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway)
crate. Radio-specific entry, proof, and restoration stay with each model.

## Identify a caller-selected connection

The `probe` feature borrows `kenwood_transport::Transport`. A caller naming
that trait also declares a direct `kenwood-transport` dependency; disable its
default features when no serial backend is needed. One absolute
deadline covers the request write and every read. Success requires a complete
protocol-1 or protocol-2 version response with a nonempty description; reads
leave bytes after that frame untouched.

```rust,no_run
# #[cfg(feature = "probe")]
async fn identify<T: kenwood_transport::Transport>(
    transport: &mut T,
) -> Result<mmdvm::core::VersionResponse, mmdvm::probe::ProbeError> {
    mmdvm::probe::probe_version(transport, std::time::Duration::from_secs(2)).await
}
```

A successful response confirms MMDVM framing on the borrowed connection only;
it does not identify the radio or distinguish Terminal from Access Point mode.
The caller decides when a probe is appropriate and how to retire or recover a
failed or cancelled exchange.

For a diagnostic-only version/status pair, use `probe::probe_diagnostics`.
One absolute deadline covers both requests and every response read. Status is
requested only after an accepted version response on the same borrowed
connection; a status error remains in `DiagnosticResponse::status` without
erasing `DiagnosticResponse::version`. The parser rejects unknown status mode
bytes instead of treating them as Idle. No modem loop or configuration is
started.
Both version probes reject malformed UTF-8 and control characters remaining
inside the description after trailing NUL/whitespace padding is removed.
Valid Unicode remains supported; a rejected description is never silently
repaired to make it pass.

`probe::probe_diagnostics_until` additionally checks a cancellation callback
before the version request and after its complete reply, before status. It
finishes an in-progress exchange or reaches the original deadline; dropping
the future directly is still not a protocol-safe cancellation boundary.
The caller owns cleanup. MMDVM has no request correlation identifier, so a
received status cannot establish when the device generated it.

## Drive and reclaim a modem

The `runtime` layer accepts any `AsyncRead + AsyncWrite + Send + Unpin`
stream. For a `kenwood_transport::Transport`, use that crate's `StreamAdapter`.
The caller must establish the binary protocol before spawning the modem.

```rust,no_run
# #[cfg(feature = "runtime")]
async fn drive<S: mmdvm::Transport + 'static>(stream: S) -> Result<S, mmdvm::ShutdownFailure> {
    let mut modem = mmdvm::AsyncModem::spawn(stream);
    if let Some(event) = modem.next_event().await {
        println!("{event:?}");
    }
    let completed = modem.shutdown().await?;
    println!("queued frames discarded: {:?}", completed.summary().dropped_frames());
    Ok(completed.into_transport())
}
```

Spawning sends `GetVersion` and `GetStatus`; the loop then polls status every
250 ms. Status is accepted only after a supported version reply. D-STAR frames
leave its bounded TX queue only when a status in that confirmed layout reports
sufficient FIFO space. Submission success means queued, not transmitted.
`Event::TxDropped` reports queued frames discarded when the loop exits.
Command identifiers use `core::Command`, including unknown wire bytes. Raw
submissions validate the 506-byte payload bound before entering the command
channel; `SET_MODE` and `SET_CONFIG` are reserved for their correlated operations.
A failed write or flush retires
the stream, since an incomplete frame cannot safely be followed by another.

`set_mode` uses one two-second deadline for channel submission and response
waiting. After a written change times out or its caller cancels, the loop rejects
another change with `ShellError::ModeChangePending` until the earlier ACK or NAK
arrives. The new call's total deadline still applies while waiting for the loop.
The caller may instead shut down and retire the connection. Received debug
events retain their command and binary payload; the debug command numbers do
not represent severity levels.

`shutdown` awaits the modem task and returns `ModemShutdown`, containing its
stream and a `ShutdownSummary`. Inspect the summary before calling
`into_transport`: it retains the authoritative queued-frame-drop count, up to
256 unread events, and the exact number of unread events the ring overwrote.
`ShutdownFailure` retains the same summary alongside its typed cause. A task
interrupted before returning its result has an unknown dropped-frame count.
For a `StreamAdapter`, also await `shutdown_and_recover` before using its inner
transport. Only then may model-specific cleanup or restoration proceed.
Shutdown itself sends no CAT exit, close, or reopen command. Dropping a handle
does not await cleanup, and a failed modem task can make its stream unrecoverable.
The shutdown drain budget is two seconds after the task accepts shutdown,
checked between operations; a write and flush can additionally take five
seconds. This is not a two-second bound on the complete shutdown future.

## Add D-STAR voice operation

Construct `DstarModemConfig::new("N0CALL")?.with_suffix("/P")?` before I/O.
Its identity fields are private: blank callsigns, over-width input, non-printable
ASCII, and commas are rejected instead of truncated or repaired. The callsign
and suffix getters return fixed-width protocol values. Receive events preserve
opaque wire bytes independently of this transmit-configuration validation.

Also construct `DstarConfiguration` with an explicit `ConfigurationProfile`,
checked `TxDelay` in ten-millisecond units, and `SignalLevel` receive/transmit
settings before mode entry. Pass the prepared `AsyncModem`, identity, and wire
configuration to `DstarModem::initialize`. It waits for a matching version,
sends D-STAR-only `SetConfig`, waits for its matching ACK, then sends correlated
`SetMode(Dstar)`. Initialization failure returns the
original modem handle alongside a typed `DstarError`. `into_modem` returns the
running handle without I/O, leaving shutdown and radio restoration to its owner.
The `mmdvm::dstar` module contains a complete initialization/recovery example.

Important operating boundaries:

- `DstarModem::next_event` returning `Ok(None)` means a quiet poll interval, not EOF.
  Closed or failed modem tasks return errors; dropped event-ring entries
  remain explicit discontinuities.
- An exact inbound URCALL echo command records voice until an observed stream
  boundary, up to `MAX_ECHO_RECORDING_FRAMES` (3,000 frames, 36,000 voice-payload
  bytes, or 60 seconds at nominal cadence). This is a frame-count bound, not
  a wall-clock timeout. The next frame discards the entire recording and emits
  `DstarEvent::EchoRecordingAborted` once, after its normal `VoiceData` event.
  Reception continues; only a new header can start another recording. A clean
  EOT within the limit automatically queues playback; an oversized recording
  never replays a truncated prefix. The recording limit does not reserve TX
  capacity: playback can fail with a typed queue error after a prefix was
  submitted. This runtime is not receive-only.
- One two-second absolute `poll_status` deadline covers request submission,
  all received events, and dispatch, including automatic echo playback.
  Continuing voice traffic cannot extend the total wait. Unrelated events
  remain queued in their original order, including an observed `VoiceEnd`
  when a timeout interrupts echo submission.
- Waiting for an event is cancellation-safe. TX submission, initialization,
  and automatic echo playback are not cancellation-atomic: already queued or
  written commands may still take effect. Cancellation is not rollback.

Only D-STAR transmission is driven by this shell today. Other digital-voice
formats have core codec support but no corresponding shell TX workflow.

## Supported configuration layouts

`DstarConfiguration` encodes the public MMDVM version 1 and version 2 layouts:
[`MMDVMHost/Modem.cpp`](https://github.com/g4klx/MMDVM-Host/blob/be804801ac33f5b5d93278a2ad1507db0c269023/Modem.cpp)
builds 23 and 37 payload bytes respectively, and
[`MMDVM/SerialPort.cpp`](https://github.com/g4klx/MMDVM/blob/57e692ad8ca380891b56a0988478ca76817e9f64/SerialPort.cpp)
requires at least 37 for its protocol-2 parser. The supported configurations
select simplex, normal signal polarity, D-STAR only, initial Idle mode, neutral
DC offsets, zero levels for disabled modes, and zero reserved fields. Signal
levels and key-up delay are explicit caller inputs. Matching a reported version
does not establish that a particular radio firmware implements either layout.
`ConfigurationProfile::KenwoodTerminal` returns
`ConfigurationError::UnsupportedProfile` during construction, before I/O; no
verified Kenwood configuration layout is supplied.

`configure_dstar` uses one two-second deadline for submission, version
confirmation, write, and ACK waiting. A version mismatch fails before any
configuration byte is written. Once written, cancellation or timeout retains
the configuration reservation: another attempt returns
`ShellError::ConfigurationPending` until the previous ACK/NAK arrives or the
connection is retired. An unwritten expired request sends no configuration.

## Reference and status

The modem loop follows
[`MMDVMHost`](https://github.com/g4klx/MMDVMHost), including periodic status and
FIFO-space gating. Portions are derived from Jonathan Naylor G4KLX's work,
2015–2026, under GPL-2.0-or-later.

Experimental API; breaking changes prioritize correctness and clarity.
Part of the [kenwood workspace](https://github.com/swiftraccoon/kenwood).
License: GPL-2.0-or-later.
