# lodestar-core

Rust core for [Lodestar](https://github.com/swiftraccoon/kenwood/tree/main/lodestar), the native macOS and iPadOS
D-STAR gateway app for the Kenwood TH-D75.

Wraps `dstar-gateway-core`, `dstar-gateway`, `mmdvm-core`, and `mbelib-rs` and
exposes them to Swift via [UniFFI](https://mozilla.github.io/uniffi-rs/). The Swift
bindings ship as `LodestarKit.xcframework`, produced by
`scripts/build-xcframework.sh`.

## What's exposed

- `version()`: crate semver.
- Audio: `RxAudioPipeline`, which AMBE-decodes reflector voice frames into 8 kHz
  mono PCM for on-device monitoring.
- CAT: `CatCommand::encode` and `CatResponse::parse`, with UDL entry points
  `encode_cat` and `parse_cat_line` for the `ID` and `FV` identity commands.
- MCP: page read/write primitives for flipping menu 650 (DV Gateway) into
  Reflector Terminal Mode.
- MMDVM: frame codec and `mmdvm_get_version_probe` for radio-mode detection.
- Reflector sessions: checked `ReflectorConnectionConfig` and async
  `ReflectorSession::connect`, with
  `send_header` / `send_voice` / `send_eot` over `DPlus` / `DExtra` / DCS, plus a
  `ReflectorObserver` callback protocol Swift implements to receive voice
  events and slow-data updates (TX text + DPRS/NMEA GPS).
- `decode_radio_header`: parse the radio's 41-byte on-wire D-STAR header
  into typed fields so Swift can synthesise a local "recently heard" entry
  for operator-originated transmissions (reflectors don't echo the sender
  back).
- Reflector directory: `default_reflectors()` plus live-directory support via
  `parse_hosts_text`, `merge_directories`, and the `DirectoryEntry` /
  `DirectorySource` types for provenance-aware merging of bundled and `DPlus`
  auth-server sources.
- Logging bridge: `init_tracing()` installs a `tracing` subscriber that
  forwards each event to a Swift-implemented `LogSink` (with `LogLevel`),
  routing Rust diagnostics into the app's Log Viewer and Apple's Unified Log.
  The [native log bridge](https://github.com/swiftraccoon/kenwood/blob/main/lodestar/Shared/Relay/RustLogBridge.swift)
  demonstrates the callback implementation. `init_tracing` uses `RUST_LOG` or
  the documented default filter; an existing global subscriber remains in place.

## Construction and completion

`Reflector` and `ReflectorConnectionCandidate` are editable foreign candidates.
`ReflectorConnectionConfig::new` checks seven-byte routing identities, module
letters, hostname/IP syntax, the nonzero UDP port and the DCS target before I/O.
The immutable configuration retains checked values throughout connection.
Setup has one 35-second wall-clock budget, including DNS, authentication and
UDP handshakes. DNS has five seconds; at most eight resolved endpoints are
attempted with a socket matching the endpoint's address family. Each UDP
handshake has five seconds. `DPlus` authentication retains its bounded TCP
phases; if it fails, UDP linking is still attempted and a combined failure
retains both details. XRF/DCS connections do not use that auth exchange.

One background task owns each connected session. Its 64-request queue accepts
a transmission when enqueueing completes. Cancellation before acceptance queues
nothing; abandoning a response afterwards permits the send to complete. A
successful send observes UDP completion, rather than remote receipt. An
uncertain send must not be retried automatically. Observer callbacks run
synchronously on that task and must return promptly without waiting for another
session operation. Slow callbacks delay network pumping and shutdown.

`disconnect` starts shutdown and awaits the task's cleanup result. Cancelling
that wait retains the task handle and original result for a later call. After
completion, repeated calls return `ReflectorError::AlreadyDisconnected`.
`is_connected` becomes false when shutdown starts or the worker finishes.
Dropping the last handle signals shutdown without waiting or observing errors.

`VoiceStreamId` rejects zero and `VoiceSequence` admits only indices 0 through
20. Their checked foreign lifting preserves Swift's `UInt16`/`UInt8` value
formats. Generated transmit indices wrap modulo 21. Raw receive bytes remain
observations and are validated before use in a new transmission.

MCP page objects retain exactly 256 bytes and expose immutable copies. Reading
can observe any 16-bit address; `write_command` refuses addresses above the
writable region. Parsing decodes the first 261-byte page and ignores trailing
bytes; the caller separately handles ACK, deadlines, retry state and reconnect.
The native MCP session checks the echoed page address and performs read-back
verification. Schema/settings, decoded GPS and version records are output
observations with private Rust fields; foreign copies confer no mutation
authority. MMDVM frame records remain editable candidates whose encoding and
core conversion validate the complete payload.

## Receive audio and positions

`RxAudioPipeline` accepts exact 12-byte voice payloads and checked superframe
indices. Malformed input returns a typed error before decoder/history/counters
change. Valid late frames return empty PCM and increase the late counter;
lookahead buffering also legitimately returns empty PCM. Loss counters saturate
at `u32::MAX`. The pipeline returns enhanced 8 kHz mono `i16` samples, preserves
20 ms frame boundaries and flushes its retained tail on `end_stream`. Embedded
enhancer initialization failure is logged and falls back to the base decoder.
Poisoned state returns `RxAudioError::StateUnavailable`; `start_stream` replaces
the entire state and clears poison before later processing.

Slow-data GPS assembly retains at most 1,024 raw bytes, including malformed
UTF-8, and resynchronizes at complete following sentences. Positions require a
valid DPRS CRC or NMEA XOR, active RMC status or GGA fix quality 1 through 6,
decimal degree/minute grammar, minute values below 60, axis-specific hemispheres
and finite geographic bounds. Coordinates at a pole or the antimeridian require
zero minutes. GPS records retain decimal degrees; malformed sentences do not
replace a previously accepted position. The integrity checks follow
[`ircDDBGateway`'s collector](https://github.com/g4klx/ircDDBGateway/blob/f5ab9f7c93c6f28147b84a8f9667f6cc4c027eaa/Common/APRSCollector.cpp).

## Pure codecs and audio

These operations need no radio, socket or physical audio device:

```rust
use lodestar_core::{CatCommand, CatResponse, McpPage, MmdvmFrame,
    MmdvmVersionResponse, RxAudioPipeline, VoiceSequence};

assert_eq!(CatCommand::Identify.encode(), b"ID\r");
assert!(matches!(CatResponse::parse(b"ID TH-D75\r"), CatResponse::Identify { .. }));
let page = McpPage::new(0x1C, vec![0; 256])?;
let changed = page.with_byte(0xA0, 1);
let recovered = McpPage::parse(changed.write_command()?)?;
assert_eq!(recovered.data(), changed.data());
assert_eq!(page.data().get(0xA0), Some(&0));

let wire = MmdvmFrame { command: 0, payload: vec![] }.encode()?;
assert_eq!(MmdvmFrame::decode(&wire)?.bytes_consumed(), 3);
let version = MmdvmVersionResponse::parse(b"\x01MMDVM 2018")?;
assert_eq!(version.protocol(), 1);

let audio = RxAudioPipeline::new();
audio.start_stream();
let mut voice = dstar_gateway_core::AMBE_SILENCE.to_vec();
voice.extend_from_slice(&[0; 3]);
let mut pcm = audio.push_voice(VoiceSequence::ZERO, voice)?;
let end = audio.end_stream()?;
pcm.extend(end.tail_pcm);
assert_eq!(pcm.len(), 160);
assert_eq!(end.stats.received, 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Directory parsing skips malformed rows, prefers hostname rows over duplicate
IP rows and preserves protocol/source identity when merging:

```rust
use lodestar_core::{DirectoryEntry, DirectorySource, ReflectorProtocol,
    default_reflectors, merge_directories, parse_hosts_text};

let mut entries: Vec<_> = default_reflectors().into_iter().map(|reflector|
    DirectoryEntry { reflector, source: DirectorySource::Bundled }).collect();
let custom = parse_hosts_text(ReflectorProtocol::DExtra,
    "XRF999 xrf999.example.org\n".to_owned());
entries.extend(custom.into_iter().map(|reflector|
    DirectoryEntry { reflector, source: DirectorySource::Bundled }));
assert!(merge_directories(entries).iter().any(|entry| entry.reflector.name == "XRF999"));
```

## Network session

The following workflow needs a Tokio runtime and the selected reflector. Its
observer intentionally discards receive events; an application replaces it with
a nonblocking event queue. Both transmission and disconnect outcomes remain
available to the caller, including simultaneous failures.

```rust,no_run
use std::sync::Arc;
use lodestar_core::{Reflector, ReflectorConnectionCandidate, ReflectorConnectionConfig,
    ReflectorError, ReflectorProtocol, ReflectorSession, VoiceSequence, VoiceStreamId};
use lodestar_core::session::{ReflectorEvent, ReflectorObserver};

#[derive(Debug)]
struct IgnoreEvents;
impl ReflectorObserver for IgnoreEvents {
    fn on_event(&self, _event: ReflectorEvent) {}
}

#[derive(Debug)]
struct Completion {
    transmission: Result<(), ReflectorError>,
    disconnect: Result<(), ReflectorError>,
}

async fn relay_one(header: [u8; 41], voice: [u8; 12]) -> Result<Completion, Box<dyn std::error::Error>> {
    let configuration = ReflectorConnectionConfig::new(ReflectorConnectionCandidate {
        callsign: "W1AW".to_owned(), local_module: "B".to_owned(),
        reflector_module: "C".to_owned(), reflector: Reflector {
            name: "XRF030".to_owned(), host: "xrf030.example.org".to_owned(),
            port: 30001, protocol: ReflectorProtocol::DExtra, description: String::new(),
        },
    })?;
    let session = ReflectorSession::connect(configuration, Arc::new(IgnoreEvents)).await?;
    let stream = VoiceStreamId::new(1)?;
    let transmission: Result<(), ReflectorError> = async {
        session.clone().send_header(header.to_vec(), stream).await?;
        session.clone().send_voice(stream, VoiceSequence::ZERO, voice.to_vec()).await?;
        session.clone().send_eot(stream, VoiceSequence::ZERO.next()).await?;
        Ok(())
    }.await;
    let disconnect = session.disconnect().await;
    Ok(Completion { transmission, disconnect })
}
```

## License

GPL-2.0-or-later OR GPL-3.0-or-later.
