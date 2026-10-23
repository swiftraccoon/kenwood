# dstar-gateway-core

Sans-io D-STAR reflector codecs, checked transmit values and client/server
session state machines for `DPlus`, `DExtra` and DCS. The caller supplies time,
sends queued datagrams and feeds received bytes back into the machine. The
library opens no sockets, starts no tasks and reads no clock.

The [client shell](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway)
provides Tokio and blocking drivers. The
[reflector server](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway-server)
provides multi-client I/O and fan-out. These experimental APIs may change
before publication.

## Drive a client

`Session<P, S>` consumes its wrapper during link transitions. A `Failed`
transition returns the session and typed error. Driver callbacks and in-place
disconnect can change runtime state later; send operations check that state
and return `StateError::WrongState` after closure or disconnect.

This executable example exchanges bytes with a codec-generated peer and
performs no network I/O:

```rust
use std::assert_matches;

use dstar_gateway_core::{
    Callsign, Configured, DExtra, Driver, DstarHeader, Event, FrameSequence,
    Module, RelayRouting, RepeaterIdentity, RoutingCallsign, Session, StreamId, Suffix, VoiceFrame,
};
use dstar_gateway_core::codec::dextra;

let station = RoutingCallsign::new("W1AW")?;
let reflector = RoutingCallsign::new("XRF030")?;
let peer = "127.0.0.1:30001".parse()?;
let now = std::time::Instant::now(); // The application's clock.
let configured = Session::<DExtra, Configured>::builder()
    .callsign(station)
    .local_module(Module::B)
    .reflector_module(Module::C)
    .peer(peer)
    .build();
let mut connecting = configured.connect(now)?;
let link = connecting.poll_transmit(now).ok_or("LINK missing")?;
assert_eq!(link.dst, peer);
assert_eq!(link.payload.len(), 11);

let mut reply = [0_u8; 14];
let local = RepeaterIdentity { callsign: station, module: Module::B };
let length = dextra::encode_connect_ack(&mut reply, local, Module::C)?;
connecting.handle_input(now, peer, reply.get(..length).ok_or("ACK length")?)?;
let mut connected = connecting.promote()?;
assert_matches!(connected.poll_event(), Some(Event::Connected { .. }));

let header = DstarHeader::for_relay(RelayRouting {
    operator: station,
    local_module: Module::B,
    reflector,
    reflector_module: Module::C,
}, Callsign::try_from_str("W1AW")?, Suffix::EMPTY);
let stream = StreamId::try_from(0x1234_u16)?;
connected.send_header(now, &header, stream)?;
connected.send_voice(now, stream, FrameSequence::ZERO, &VoiceFrame::silence())?;
connected.send_eot(now, stream, FrameSequence::ZERO.next())?;
let mut packets = Vec::new();
while let Some(transmit) = connected.poll_transmit(now) {
    packets.push(transmit.payload.to_vec());
}
assert_eq!(packets.iter().map(Vec::len).collect::<Vec<_>>(), [56, 27, 27]);
assert!(connected.poll_timeout().is_some());
# Ok::<(), Box<dyn std::error::Error>>(())
```

An I/O driver drains ready transmits and events before waiting for input or
`Driver::poll_timeout`, then calls `handle_timeout` when that deadline is
due. Datagrams from another peer are ignored. Unknown or malformed reflector
traffic is logged and ignored; structured recoverable observations are also
available through `Session::diagnostics`. Equal release times preserve FIFO
packet order. A caller must keep draining the unbounded sans-io queues.

DCS control packets carry their target identity. Its builder additionally
requires `.dcs_reflector(DcsReflector::new("DCS030")?)`; omitting that setter
prevents `build`. Direct runtime callers use `SessionCore::new(SessionConfig)`
with `ClientProtocol::Dcs(target)`. No target identity is inferred from an IP
address or substituted by default.

Clock-bearing entry points check protocol deadline representability.
`enqueue_connect`, `enqueue_disconnect` and `handle_input` return
`StateError::DeadlineOverflow` before changing state when the supplied instant
is too close to the platform limit. `handle_timeout` instead closes once with
`DisconnectReason::ClockRange` and clears pending timers and transmits.

## Drive a server

`ServerSessionCore` handles one configured peer and reflector module. The shell
routes each source address to its own instance and performs authorization.
LINK ordering, stable linked identity and active-stream matching are checked
at runtime. A closed session ignores subsequent input.

```rust
use dstar_gateway_core::{
    DExtra, Module, ProtocolKind, RepeaterIdentity, RoutingCallsign, ServerEvent, ServerSessionCore,
};
use dstar_gateway_core::codec::dextra;

let now = std::time::Instant::now();
let peer = "127.0.0.1:30001".parse()?;
let station = RoutingCallsign::new("W1AW")?;
let mut server = ServerSessionCore::new(ProtocolKind::DExtra, peer, Module::C);
let mut request = [0_u8; 11];
let local = RepeaterIdentity { callsign: station, module: Module::B };
let length = dextra::encode_connect_link(&mut request, local, Module::C)?;
server.handle_input(now, request.get(..length).ok_or("LINK length")?)?;
assert!(matches!(server.pop_event::<DExtra>(), Some(ServerEvent::ClientLinked { .. })));
let ack = server.pop_transmit(now).ok_or("ACK missing")?;
assert_eq!(ack.dst, peer);
assert_eq!(ack.payload.len(), 14);
# Ok::<(), Box<dyn std::error::Error>>(())
```

`pop_event::<P>` returns `None` without consuming an event if `P` differs from
the stored protocol. Unknown datagram lengths are ignored. Other fatal codec
errors return `Error::Protocol`. A LINK with an invalid station prefix returns
`Error::Type` before accepting that identity. The I/O driver reports these errors.

## Wire bytes and checked transmit fields

`Callsign::from_wire_bytes`, `Suffix::from_wire_bytes`, `DstarHeader::decode`
and decoded packet records preserve receive fields. Checked `text()` views
borrow printable ASCII; `Display` escapes nonprintable bytes for diagnostics.
The directional codecs accept a caller's output buffer and return its written
length or a typed error. Their decoder results retain the entire sequence
byte, including EOT and unknown bits, with diagnostics for recoverable faults.

New transmit frame indices use `FrameSequence`, restricted to `0..=20`.
`next()` wraps to zero; the encoder adds termination bits separately. Repeater
routing uses `RoutingCallsign`, which reserves the eighth byte for a module
and rejects text that would be truncated. `HeaderFlags` preserves three wire
bytes and provides independent `HeaderFlag` operations plus a mutually
exclusive `RepeaterControl` field. DSVT encoding zeroes flags before computing
its CRC; DCS encoding preserves the original flags.

Module-bearing control encoders take `RepeaterIdentity`, keeping its checked
station prefix and client module together. ACK/NAK decoders retain the echoed
client module. Clients match station, local module, target module and handshake
phase before completing a link or disconnect. `ModuleSlot::Empty` represents
the space target used by NAK and unlink replies; a delayed LINK ACK never
completes UNLINK.

## D-PRS, slow data and host lists

```rust
use dstar_gateway_core::{
    DprsComment, DprsPosition, DprsReport, DprsSource, DprsSymbol, Latitude,
    Longitude, SlowDataTextMessage, SymbolCode, SymbolTable,
};

let report = DprsReport::new(
    DprsSource::new("W1AW")?,
    DprsPosition { latitude: Latitude::new(35.5)?, longitude: Longitude::new(-82.55)? },
    DprsSymbol::new(SymbolTable::Primary, SymbolCode::new('>')?),
    DprsComment::new("Asheville test")?,
);
let sentence = report.to_sentence();
let received: DprsReport = sentence.parse()?;
assert_eq!(received, report);
let message = SlowDataTextMessage::try_from("W1AW reflector test")?;
assert_eq!(message.encode().len(), 8); // Eight scrambled three-byte chunks.
# Ok::<(), Box<dyn std::error::Error>>(())
```

`DprsReport::parse` takes an explicit `ChecksumPolicy`; `FromStr` verifies
the CRC by default. The checksum includes the terminating carriage return.
Diagnostic checksum bypass still validates source, coordinate grammar,
symbol selectors and plain comments. Coordinates round to hundredths of a
minute when encoded. Plain comments hold at most 43 printable ASCII bytes,
excluding `|` and `~`; other APRS extensions need their own parser.

`SlowDataAssembler` reconstructs blocks from paired received chunks.
`SlowDataTextCollector` combines the four fixed five-byte text blocks into a
20-byte message. Both preserve receive bytes and expose checked text views.
`echo::EchoUnit` records a bounded stream and returns its replay frames after
an injected-time delay; the caller paces and transmits that replay.

`hosts::HostFile` loads and merges typed `HostEntry` values. Names normalize to
uppercase catalog tokens, addresses are checked IP literals or DNS names,
and ports are nonzero. Parsing stages every row before changing the existing
map; a malformed row returns its one-based line and typed cause. Borrowed and
consuming iteration, `Extend` and `FromIterator` share last-entry-wins behavior.
See the executable `HostFile` and `DprsReport` module examples for file merging
and diagnostic parsing. Catalog names require separate validation before use
as filesystem components.

## Protocol references and validation

Wire comparisons use [ircDDBGateway revision
`f5ab9f7`](https://github.com/g4klx/ircDDBGateway/tree/f5ab9f7c93c6f28147b84a8f9667f6cc4c027eaa)
and [xlxd revision
`bf5d014`](https://github.com/LX3JL/xlxd/tree/bf5d0148dbdf2534af129ca3cc034c5051dcfc8d).
D-PRS position grammar follows [APRS 1.0.1](https://www.aprs.org/doc/APRS101.PDF)
and the pinned `Common/APRSCollector.cpp` checksum and source formatting.
These references establish reflector and packet behavior; model crates
document radio firmware and interface contracts separately.

The workspace [contributor workflow](https://github.com/swiftraccoon/kenwood/blob/main/CONTRIBUTING.md)
defines validation. Unit, golden-wire, property, compile-fail and doctest
suites run on pinned Rust 1.99.1. The independent fuzz workspace has ten
targets, its own inherited lint policy and formatter check. Stable fuzz
compilation and nightly fuzz execution are separate checks.
