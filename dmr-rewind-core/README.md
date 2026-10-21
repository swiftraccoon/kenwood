# dmr-rewind-core

`dmr-rewind-core` is a sans-I/O codec for the `BrandMeister` REWIND UDP
protocol. It validates and emits complete datagrams, preserves packet types it
does not yet model, parses DMR call metadata, and provides the protocol's
authentication digest. It owns no socket, clock, retry loop, login flow, or
session state machine.

## Access mode

This crate supports `BrandMeister` Open DMR Terminal: service byte `0x21` on UDP
port `54006`, authenticated with the user's seven-digit DMR ID and that ID's
Hotspot Security password. It deliberately does not implement the separately
provisioned Simple External Application service.

Unexpected REWIND control packet types retain their names and opaque bodies.

## Wire model

Every datagram starts with this exact 18-byte little-endian envelope:

```text
offset  size  field
0       8     ASCII signature "REWIND01"
8       2     packet type (u16)
10      2     flags (u16)
12      4     sequence (u32)
16      2     payload length (u16)
18      n     payload
```

The declared payload length must equal the remaining datagram length. The
decoder rejects a bad signature, a truncated payload, trailing bytes, an
oversized UDP datagram, and a modeled payload with the wrong fixed size.
Unknown packet types and packet classes without a modeled body remain
available as opaque bytes.

The codec accepts these media payload sizes:

- DMR voice header full link control: 12 bytes
- DMR terminator: 0 bytes, or 12 bytes with full link control
- DMR audio burst: 27 bytes
- Super-header metadata: 32 bytes

DMR embedded data (`0x0927`) remains uninterpreted bytes, subject to the outer
datagram limit. The datagram limit is 65,507 bytes, including the REWIND
envelope, calculated from IPv4's 65,535-byte total length minus its minimum
20-byte header and UDP's eight-byte header ([RFC 791][ipv4], [RFC 768][udp]).

Most multi-byte protocol fields are little-endian. The two DMR IDs inside a
12-byte full-link-control body are exceptions: destination bytes 3 through 5
and source bytes 6 through 8 are unsigned 24-bit big-endian values. In its
first control octet, bit 7 is the protection flag, bit 6 is reserved, and the
low six bits are FLCO. The codec classifies FLCO `0` as group voice and FLCO
`3` as private voice while preserving the complete raw control octet.

Authentication is:

```text
SHA-256(challenge bytes || password bytes)
```

The resulting 32 bytes form the authentication payload.

## Construct and round-trip a packet

`Packet::new` checks the payload variant and length before retaining the owned
payload. A packet's envelope and payload cannot subsequently be replaced.
Encoding allocates one output vector and cannot fail for a constructed packet.

```rust
use dmr_rewind_core::{Packet, PacketFlags, PacketType, Payload, SessionType, Subscription};

let packet = Packet::new(
    PacketType::Subscription,
    PacketFlags::REAL_TIME_1 | PacketFlags::from_bits(0x8000),
    42.into(),
    Payload::Subscription(Some(Subscription {
        session_type: SessionType::GroupVoice,
        target: 91.into(),
    })),
)?;
let bytes = packet.to_bytes();
assert_eq!(packet.header().payload_len, 8);
assert_eq!(Packet::from_bytes(&bytes)?, packet);
assert!(packet.header().flags.contains(PacketFlags::REAL_TIME_1));
assert_eq!(format!("{:04x}", packet.header().flags), "8001");
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Validated wire values

The classifiers preserve every raw packet or session code. Explicit unknown
values reject named codes, so one wire value has one interpretation. The audio
and data subtype constructors reject values outside their families.

```rust
use dmr_rewind_core::{
    DmrAudioSubtype, DmrDataSubtype, FullLinkControlType, PacketType, SessionType,
    UnknownFullLinkControlType, UnknownPacketType, UnknownSessionType,
};

let audio = DmrAudioSubtype::try_from(4)?;
assert_eq!(PacketType::DmrAudio(audio).as_raw(), 0x0924);
assert!(DmrAudioSubtype::new(7).is_err());
assert!(DmrDataSubtype::new(1).is_err()); // Named voice-header code.
let unknown = UnknownPacketType::new(0xbeef)?;
assert_eq!(PacketType::from_raw(0xbeef), PacketType::Unknown(unknown));
assert!(UnknownPacketType::new(0x0911).is_err());
let session = UnknownSessionType::new(0x1234_5678)?;
assert_eq!(SessionType::Unknown(session).as_raw(), 0x1234_5678);
let opcode = UnknownFullLinkControlType::new(9)?;
assert_eq!(FullLinkControlType::from_flco(0xc9), FullLinkControlType::Unknown(opcode));
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Full link control

`DmrId` validates the three-byte wire width, including zero. It does not check
whether a subscriber or talkgroup has been assigned that identifier. Raw
control flags and the final three bytes survive a round trip unchanged.

```rust
use dmr_rewind_core::{DmrId, FullLinkControl, FullLinkControlType};

let control = FullLinkControl {
    flco: 0xc3.into(),
    feature_id: 0.into(),
    service_options: 4.into(),
    destination_id: DmrId::new(91)?,
    source_id: DmrId::new(3_101_234)?,
    tail: [1, 2, 3],
};
assert_eq!(control.call_type(), FullLinkControlType::Private);
let bytes = control.to_bytes();
assert_eq!(FullLinkControl::from_bytes(bytes), control);
assert_eq!(format!("{:06x}", control.source_id), "2f5232");
assert!(DmrId::new(0x0100_0000).is_err());
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Call metadata

Super-header identifiers preserve their complete four-byte fields. Callsigns
preserve all ten bytes, including invalid UTF-8 and padding. The optional
lossy text conversion stops at the first NUL and removes trailing Unicode
whitespace. Consuming accessors transfer packet storage without cloning.

```rust
use dmr_rewind_core::{Callsign, Packet, PacketFlags, PacketType, Payload, SessionType, SuperHeader};

let metadata = SuperHeader {
    session_type: SessionType::GroupVoice,
    source_id: 3_101_234.into(),
    target_id: 91.into(),
    source_call: Callsign::from_bytes(*b"W1AW  \0xyz"),
    target_call: Callsign::from_bytes(*b"WORLD     "),
};
assert_eq!(metadata.source_call.to_trimmed_lossy(), "W1AW");
assert_eq!(metadata.source_call.to_bytes(), *b"W1AW  \0xyz");
let packet = Packet::new(PacketType::SuperHeader, PacketFlags::NONE, 1.into(), Payload::SuperHeader(metadata))?;
assert_eq!(packet.payload(), &Payload::SuperHeader(metadata));
let (header, payload) = packet.into_parts();
assert_eq!(header.payload_len, 32);
assert_eq!(payload, Payload::SuperHeader(metadata));
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Authentication

The digest uses exact challenge and password bytes. The codec does not retain
the password or perform the login exchange.

```rust
use dmr_rewind_core::{
    Packet, PacketFlags, PacketType, Payload, SERVICE_OPEN_TERMINAL, VersionData,
    authentication_digest,
};

let hello = Packet::new(PacketType::KeepAlive, PacketFlags::NONE, 0.into(), Payload::KeepAlive(Some(
    VersionData {
        remote_id: 3_101_234.into(),
        service: SERVICE_OPEN_TERMINAL,
        description: b"receive-client/0.1".to_vec(),
    },
)))?;
let digest = authentication_digest(b"challenge", b"example-password");
let response = Packet::new(PacketType::Authentication, PacketFlags::NONE, 1.into(),
    Payload::Authentication(digest))?;
assert_eq!(response.header().payload_len, 32);
assert!(matches!(hello.into_payload(), Payload::KeepAlive(Some(_))));
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Session polls

Poll words retain their full 32-bit values without assigning meanings to
unmodeled selectors or flags.

```rust
use dmr_rewind_core::{Packet, PacketFlags, PacketType, Payload, SessionPoll};

let poll = SessionPoll {
    kind: 1.into(), flags: 0x8000_0000.into(), number: 91.into(), state: 0.into(),
};
let packet = Packet::new(PacketType::SessionPoll, PacketFlags::NONE, 2.into(), Payload::SessionPoll(poll))?;
assert_eq!(Packet::from_bytes(&packet.to_bytes())?.into_payload(), Payload::SessionPoll(poll));
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Media payloads

Fixed arrays enforce modeled audio and full-link-control lengths before packet
construction. Embedded data, other data subtypes and unknown packet codes
preserve uninterpreted byte vectors.

```rust
use dmr_rewind_core::{
    DMR_AUDIO_LEN, DmrAudioSubtype, DmrDataSubtype, Packet,
    PacketFlags, PacketType, Payload,
};

let audio = Packet::new(PacketType::DmrAudio(DmrAudioSubtype::new(0)?),
    PacketFlags::REAL_TIME_1, 3.into(), Payload::DmrAudio([0; DMR_AUDIO_LEN]))?;
let embedded = Packet::new(PacketType::DmrEmbeddedData, PacketFlags::REAL_TIME_1, 4.into(),
    Payload::DmrEmbeddedData(vec![1, 2, 3]))?;
let data = Packet::new(PacketType::DmrData(DmrDataSubtype::new(6)?),
    PacketFlags::REAL_TIME_1, 5.into(), Payload::Opaque(vec![1, 2, 3]))?;
let end = Packet::new(PacketType::DmrTerminator, PacketFlags::REAL_TIME_1, 6.into(),
    Payload::DmrTerminator(None))?;
for packet in [audio, embedded, data, end] {
    assert_eq!(Packet::from_bytes(&packet.to_bytes())?, packet);
}
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## Rejected input

Errors identify the rejected code, field or byte count. `Packet::from_bytes`
requires exactly one complete datagram, including the declared body.

```rust
use dmr_rewind_core::{CodecError, HEADER_LEN, Packet, PacketFlags, PacketType, Payload};

assert_eq!(Packet::from_bytes(&[]), Err(CodecError::DatagramTooShort {
    actual: 0, minimum: HEADER_LEN,
}));
assert_eq!(Packet::new(PacketType::Close, PacketFlags::NONE, 0.into(), Payload::Challenge(vec![])),
    Err(CodecError::PayloadTypeMismatch { packet_type: PacketType::Close }));
let packet = Packet::new(PacketType::Close, PacketFlags::NONE, 0.into(), Payload::Close)?;
let mut bytes = packet.to_bytes();
bytes.push(0);
assert_eq!(Packet::from_bytes(&bytes), Err(CodecError::TrailingBytes { trailing: 1 }));
# Ok::<(), dmr_rewind_core::CodecError>(())
```

## API changes

Wire fields use distinct lossless types: `RemoteId`, `WireDmrId`,
`TransportSequence`, `ServiceCode`, `PollKind`, `PollState`, `ControlOctet`,
`FeatureId`, `ServiceOptions` and `PollFlags`. Construct with `new` or `From`
and extract with `get` (`bits` for flags). These wire domains preserve every
value of their width, including unknown codes, future flags and zero. The
shell's subscriber/destination policies remain separate from received data.
`WireDmrId` preserves all 32 bits; full-link-control `DmrId` remains 24-bit.

Packets expose `header()` and `payload()` for inspection, and `into_parts()`
or `into_payload()` for ownership transfer. Use `Packet::from_bytes` and
`Packet::to_bytes` for the datagram conversion. Full-link-control identifiers
use `DmrId`; audio/data subtypes and explicitly unknown codes use validated
wrappers. `Callsign` uses `from_bytes`, `to_bytes` and `to_trimmed_lossy`.
Embedded-data bodies retain variable byte vectors without an assumed fixed size.

## Protocol references and licensing

This crate independently implements public protocol behavior. The
MIT-licensed `BrandMeister` [`go-brandmeister` REWIND package][bm-go] was used
as the structural interoperability reference. Public wire constants and Open
Terminal behavior were also cross-checked against `BrandMeister`
[`DigestPlay`'s `Rewind.h`][bm-header] and DJ4CK's
[`pyspot_rx.py`][pyspot]. Those latter repositories are GPL-3.0 references;
their source code is not incorporated into this crate.

The Rust crate itself is licensed GPL-2.0-or-later.

## Development

From the repository root:

```bash
cargo fmt --manifest-path dmr-rewind-core/Cargo.toml -- --check
cargo test --manifest-path dmr-rewind-core/Cargo.toml
cargo clippy --manifest-path dmr-rewind-core/Cargo.toml --all-targets -- -D warnings
```

[bm-go]: https://github.com/BrandMeister/go-brandmeister/tree/master/rewind
[bm-header]: https://github.com/BrandMeister/DigestPlay/blob/master/Rewind.h
[pyspot]: https://github.com/abo4/pyspot_rx/blob/main/pyspot_rx.py
[ipv4]: https://www.rfc-editor.org/rfc/rfc791.html#section-3.1
[udp]: https://www.rfc-editor.org/rfc/rfc768.html
