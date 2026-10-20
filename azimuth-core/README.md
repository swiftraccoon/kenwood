# azimuth-core

`azimuth-core` is the independent Rust control engine for Azimuth, the native
iPadOS and macOS TH-D75 app.

The Apple app owns USB discovery and implements the generated asynchronous
`ByteTransport` protocol. On macOS, the core also exposes a bounded native
Bluetooth SPP byte transport selected by exact paired address or a previously
proved USB serial identity. Rust owns the protocol stream and exposes:

- exact CAT identity `1.03.AZM`, automation-runtime qualification, and a refusal
  canary before control starts;
- authenticated 240x180 screen capture in RGB565LE and display-ready RGBA8888;
- one-use screen leases for guarded front-panel taps, followed by mandatory
  post-tap recapture;
- all 400 MCP-D75 setting records from the authoritative generated
  `kenwood-thd75` registry;
- typed live setting reads using one minimal sparse MCP session for 399 scalar
  records, with the power-on bitmap deferred from the generic path until a
  specialized editor is supplied;
- pure plan validation for boolean, integer, text, byte, enum, and finite
  choice values;
- explicit specialized presentation for the bitmap and scaled GPS coordinate
  fields, with checked raw-storage and display-seconds conversion helpers;
- batch execution of reviewed changes for ordinary settings with typed
  preconditions, an exact full-page compare before any write, and a fresh
  post-exit radio read before final values are reported; Menu 650 and Menu 980
  remain behind dedicated disruptive lifecycles;
- host-owned APRS KISS start/stop, packet-parameter configuration, continuous
  receive draining, incremental decoded/raw activity, heard-station snapshots,
  and one-shot message or manual-position transmission;
- IF-DSP radio-state reservation, read-back-verified setup, and best-effort
  restoration with explicit failure reporting, plus a bounded processor that
  measures real 48 kHz mono PCM through USB/LSB/CW/AM demodulation, returning
  calibrated spectrum snapshots, signal levels, clipping and accounting
  counters.

Automation CAT control, APRS KISS, and IF-DSP radio setup are mutually exclusive
owners of the serial stream. Stopping APRS attempts to requalify automation.
Stopping IF-DSP attempts to restore the saved radio state, and automation
resumes only after that succeeds. APRS message transmission is a one-shot
operation with no retry or acknowledgement correlation. IF-DSP retuning and
frequency restoration use a bounded sequence of individually read-back-
verified UP/DW steps; direct FO/FQ frequency writes remain quarantined.

## Setting change safety

`read_setting_values` retains the complete MCP pages behind an opaque snapshot
identifier. Every accepted `SettingChange` repeats that identifier, the value
shown during review, and the desired value. `apply_setting_changes` validates
the complete batch before I/O, checks the typed preconditions, and uses the
TH-D75 compare-and-exchange page primitive. That primitive reads every affected
live page and compares every byte before starting the first write. A radio
setting that changed after the snapshot therefore causes zero writes.

All typed values read from or written to the controller are the exact raw
storage values, including storage-representable off-menu snapshot values that
remain unavailable to the write planner. A catalog record with `ScaledInteger` presentation must use
`decode_setting_display_value` and `encode_setting_display_value` for its
user-facing seconds value. A `Blob` record or a disruptive-lifecycle setting
requires its own editor.

The TH-D75 cannot make a multi-page write physically atomic. If USB fails after
one page has been written and verified, `AutomationError::SettingsApply` retains
the failure phase, possibly written pages and independent cleanup failure.
Post-exit refresh and final-value verification have distinct phases; decoded
live values are retained even when final verification rejects them.

## Foreign inputs and observations

`AprsSessionConfig` and `IfDspConfiguration` are editable foreign candidates.
Validation produces checked Rust configuration before queueing a mode change or
replacing DSP state. APRS identity, path, symbol and all KISS timings are checked
together. `AprsKissInterface` selects USB-C or Bluetooth without an open integer
domain. The recovery constructor validates the expected CAT serial on every
platform before retaining an operation.

`ScreenLeaseId` and `SettingSnapshotId` have nonzero Rust representations and
checked foreign lifting. Swift receives `UInt64` aliases; zero is rejected at
the Rust boundary. Nonzero candidates still require an exact match against the
actor's retained frame or setting snapshot. The actor derives guarded input
from its retained checked frame, never from foreign pixel or checksum fields.
Catalog fields, choices and transforms are immutable objects with getters and
no foreign constructors. Catalog construction rejects unknown groups and
unrepresentable image geometry with `SettingCatalogError`.
Returned screen records expose immutable Rust views; foreign record copies
carry observations, rather than mutation authority. Controller identifiers stop
at exhaustion and require reconnecting; they never wrap into an old token.

The APRS journal and heard-station map each retain at most 1,000 entries. Journal
identifiers increase until `u64::MAX`. Further receipts use sequence zero and
are not retained; dropped-activity counters and `history_truncated` report that
loss. New sessions are refused after identifier exhaustion. Snapshot polling
retains the last allocated sequence. Received speeds preserve fractional knots.

`IfDspProcessor::process_pcm` accepts at most 48,000 real mono samples per call,
at 48 kHz, within `-1..=1`. It rejects non-finite or out-of-range input before
changing filter history or measurements. Its result contains measurements and
an optional spectrum. Counters saturate at `u64::MAX`; reset clears stream
history and counters while retaining configuration. A poisoned DSP mutex returns
`IfDspError::StateUnavailable`. APRS observation polling recovers its journal
mutex so retained status remains inspectable after a panic; the actor remains
the sole writer and protocol operations require their live mode state.

## Cancellation and completion

One actor serializes radio commands through a queue of eight requests. Dropping
the Rust send future while it waits for capacity leaves that request unaccepted. Once
accepted, a settings write, guarded input or packet send may complete after its
response future is abandoned. RF sends are one-shot writes; callers must not
retry an uncertain outcome as if nothing was sent. APRS start checks abandoned
responses before consuming its snapshot and restores CAT if its accepted start
result can no longer be delivered.

DV Gateway and APRS current-mode recovery expose explicit synchronous cancel
methods. A cancellation before their MCP write gate prevents mutation; after
the gate, verification and cleanup finish before the operation reports its
result. Ordinary response abandonment is distinct from those cancel signals.
Explicit `AutomationController::close` waits for accepted commands, IF-state
restoration and transport close. Object destruction makes a nonblocking shutdown
request; a closed command channel also triggers actor cleanup. Call explicit
close when its completion and errors are needed.

`ByteTransport` implementers own native resource lifetime and serialized I/O.
Writes accept the entire buffer or fail with a potentially written prefix;
implicit retries are forbidden. Reads wait for bytes or EOF and respect the
requested maximum. Empty operations are no-ops. Protocol deadlines may abandon
callbacks, so native resources must remain valid until those callbacks stop.
Close wakes parked reads and is idempotent; reopen selects the same physical
endpoint. Platform callback failures retain their typed cause in Rust transport
errors.

## Rust workflows

Validate foreign candidates and process a bounded PCM block without a radio:

```rust
use azimuth_core::{AprsSessionConfig, IfDspConfiguration, IfDspMode, IfDspProcessor, SettingValueKind, setting_catalog};

let fields = setting_catalog()?;
let beep = fields.iter().find(|field| field.id() == "radio.Beep").ok_or("beep field")?;
assert_eq!(beep.value_kind(), SettingValueKind::Boolean);

let candidate = AprsSessionConfig::default();
let checked = candidate.validate()?;
assert!(checked.source().is_none()); // Receive-only configuration.
assert_eq!(checked.path().len(), 2);

let processor = IfDspProcessor::new(IfDspConfiguration {
    mode: IfDspMode::Usb,
    filter_hz: Some(2_400.0),
})?;
let measurements = processor.process_pcm(vec![0.0; 480])?;
assert_eq!(measurements.input_sample_count, 480);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Connect through an application-provided transport and retain capture and close
outcomes independently:

```rust,no_run
use std::sync::Arc;
use azimuth_core::{AutomationController, AutomationError, ByteTransport, RemoteScreenFrame};

struct CaptureCompletion {
    capture: Result<RemoteScreenFrame, AutomationError>,
    close: Result<(), AutomationError>,
}

async fn capture(transport: Arc<dyn ByteTransport>) -> Result<CaptureCompletion, AutomationError> {
    let controller = AutomationController::connect(transport).await?;
    let capture = controller.clone().capture_screen().await;
    let close = controller.close().await;
    Ok(CaptureCompletion { capture, close })
}
```

The transport must already select the intended radio. Connection performs the
identity/runtime checks above; the example requires the named modified firmware
and a live device. Pure catalog queries and display conversions are available
through `setting_catalog`, `decode_setting_display_value` and
`encode_setting_display_value`; setting changes refer to the same checked
snapshot and expected values returned by `read_setting_values`.

## Build

Run `scripts/build-xcframework.sh` on macOS with the listed Rust Apple targets
installed. It creates `azimuth/AzimuthCore.xcframework` with iPadOS device,
iPadOS simulator, and native macOS slices, then writes the generated Swift
binding source to `azimuth/Generated`.

## License

GPL-2.0-or-later.
