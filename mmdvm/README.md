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

This proves MMDVM framing only on the borrowed connection. It does not prove
a particular radio identity, distinguish Terminal from Access Point mode, or
authorize transmission. The caller decides when a probe is appropriate and
how to retire or recover a failed or cancelled exchange.

For a diagnostic-only version/status pair, use `probe::probe_diagnostics`.
One absolute deadline covers both requests and every response read. Status is
requested only after accepted version evidence on the same borrowed connection;
a status error remains in `DiagnosticResponse::status` without erasing
`DiagnosticResponse::version`. The parser rejects unknown status mode bytes
instead of treating them as Idle. No modem loop or configuration is started.
Both version probes reject malformed UTF-8 and control characters remaining
inside the description after trailing NUL/whitespace padding is removed.
Valid Unicode remains supported; input is never repaired silently for admission.

`probe::probe_diagnostics_until` additionally checks a cancellation callback
before the version request and after its complete reply, before status. It
finishes an in-progress exchange or reaches the original deadline; dropping
the future directly is still not a protocol-safe cancellation boundary.
The caller owns cleanup. MMDVM has no request correlation identifier, so a
received status cannot establish when the device generated it. These APIs
do not establish radio identity, operating-mode selection, or RF-silent routing.

## Drive and reclaim a modem

The `runtime` layer accepts any `AsyncRead + AsyncWrite + Send + Unpin`
stream. For a `kenwood_transport::Transport`, use that crate's `StreamAdapter`.
The caller must establish the binary protocol before spawning the modem.

```rust,no_run
# #[cfg(feature = "runtime")]
async fn drive<S: mmdvm::Transport + 'static>(stream: S) -> Result<S, mmdvm::ShellError> {
    let mut modem = mmdvm::AsyncModem::spawn(stream);
    if let Some(event) = modem.next_event().await {
        println!("{event:?}");
    }
    modem.shutdown().await
}
```

Spawning sends `GetVersion` and `GetStatus`; the loop then polls status every
250 ms. D-STAR frames leave its bounded TX queue only when the modem reports
sufficient FIFO space. Submission success means queued, not transmitted.
`Event::TxDropped` reports queued frames discarded during shutdown or failure.

`shutdown` awaits the modem task and returns its stream when recovery succeeds.
For a `StreamAdapter`, also await `shutdown_and_recover` before using its inner
transport. Only then may model-specific cleanup or restoration proceed.
Shutdown itself sends no CAT exit, close, or reopen command. Dropping a handle
does not await cleanup, and a failed modem task can make its stream unrecoverable.

## Add D-STAR voice operation

Construct `DstarModemConfig::new("N0CALL")?.with_suffix("/P")?` before I/O.
Its identity fields are private: blank callsigns, over-width input, non-printable
ASCII, and commas are rejected instead of truncated or repaired. The callsign
and suffix getters return fixed-width protocol values. Receive events preserve
opaque wire bytes independently of this transmit-configuration validation.

Pass a prepared `AsyncModem` and that configuration to
`DstarModem::initialize`. It sends D-STAR-only `SetConfig`, waits for its matching
ACK, then sends correlated `SetMode(Dstar)`. Initialization failure returns the
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

## Reference and status

The modem loop follows
[`MMDVMHost`](https://github.com/g4klx/MMDVMHost), including periodic status and
FIFO-space gating. Portions are derived from Jonathan Naylor G4KLX's work,
2015–2026, under GPL-2.0-or-later.

Experimental API; breaking changes prioritize correctness and clarity.
Part of the [kenwood workspace](https://github.com/swiftraccoon/kenwood).
License: GPL-2.0-or-later.
