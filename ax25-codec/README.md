# ax25-codec

AX.25 modulo-8 frame encoding and decoding for Rust 1.99 and later.
The crate uses `no_std` plus `alloc` and performs no I/O or clock reads.

## Scope

`Ax25Packet` represents Information, Supervisory, and Unnumbered frames with a
one-octet control field. `Ax25Header` names the destination, source, repeater
path, and command/response classification. `Callsign`, `Ssid`, and
`Ax25SequenceNumber` validate their domains at construction; `DigipeaterPath`
limits paths to eight entries. `Ax25Pid` represents assigned, unassigned, and
escaped two-octet identifiers.

The byte codec accepts and emits address, control, PID, and information fields.
Flags, FCS validation, bit stuffing, and modulo-128 control fields belong to the
caller. KISS framing is provided by
[`kiss-tnc`](https://github.com/swiftraccoon/kenwood/tree/main/kiss-tnc), and APRS
information parsing by
[`aprs`](https://github.com/swiftraccoon/kenwood/tree/main/aprs).

## Encoding and decoding a packet

Use named header fields to distinguish source and destination, then construct
the frame family. UI frames carry both a PID and an information field.

```rust
use ax25_codec::{
    Ax25Error, Ax25Header, Ax25Packet, Ax25Pid, CommandResponse, DigipeaterPath,
    PollFinal,
};

let header = Ax25Header {
    destination: "APRS".parse()?,
    source: "N0CALL-7".parse()?,
    digipeaters: DigipeaterPath::empty(),
    command_response: CommandResponse::Command,
};
let packet = Ax25Packet::unnumbered_information(
    header,
    PollFinal::Clear,
    Ax25Pid::NoLayer3,
    b"hello".to_vec(),
);
let bytes = packet.to_bytes();
assert_eq!(Ax25Packet::from_bytes(&bytes)?, packet);
# Ok::<(), Ax25Error>(())
```

`from_bytes` owns copies of callsigns and information bytes. Callers that process
untrusted input impose an input-length limit before decoding; the codec bounds
the repeater count but imposes no information-length limit.

The decoder accepts lowercase wire callsigns and normalizes them to uppercase.
It ignores reserved SSID bits; encoding sets those bits to one. Address-extension
bits must occur only at the final address octet. Both legacy equal-C-bit forms
retain their distinct `CommandResponse` variants through encoding and decoding.

## Addresses and paths

`Callsign::new` and `TryFrom` reject lowercase letters; the explicit
`new_case_insensitive` constructor accepts and uppercases them. Textual
`Ax25Address` parsing accepts canonical uppercase callsigns and nonzero SSID
suffixes without leading zeros. SSID zero is displayed without a suffix.

```rust
# extern crate alloc;
use alloc::string::String;
use alloc::vec;
use ax25_codec::{Ax25Address, Ax25Error, Callsign, DigipeaterPath, RouteEntry, Ssid};

let callsign = Callsign::try_from(String::from("N0CALL"))?;
let address = Ax25Address::from_parts(callsign, Ssid::try_from(7)?);
assert_eq!(address, "N0CALL-7".parse()?);
assert!("N0CALL-07".parse::<Ax25Address>().is_err());

let mut path = DigipeaterPath::try_from(vec![RouteEntry::new("WIDE1", 1)?])?;
path.try_insert(1, RouteEntry::new("WIDE2", 1)?.marked_used())?;
assert_eq!(path.iter().count(), 2);
assert!(path.get(1).is_some_and(|entry| entry.has_repeated));
# Ok::<(), Ax25Error>(())
```

Path insertion and append return `Ax25Error::TooManyDigipeaters` at capacity
without changing the path. `marked_used` consumes an entry and retains its
callsign allocation; clone first when the original entry is needed.

## Control fields and protocol identifiers

Typed control fields keep sequence numbers and poll/final state separate.
An escaped PID contains both the `0xff` prefix and its required extension.

```rust
use ax25_codec::{Ax25Control, Ax25Error, Ax25Pid, Ax25SequenceNumber, PollFinal};

let control = Ax25Control::Information {
    ns: Ax25SequenceNumber::try_from(3)?,
    nr: Ax25SequenceNumber::try_from(6)?,
    pf: PollFinal::Set,
};
assert_eq!(u8::from(control), 0xd6);
assert_eq!(Ax25Control::from(0xd6), control);

let pid = Ax25Pid::escaped(0xcc);
assert_eq!(pid.as_byte(), 0xff);
assert_eq!(pid.extension_byte(), Some(0xcc));
assert_eq!(pid.wire_len(), 2);
assert_eq!(
    Ax25Pid::try_from(0xff),
    Err(Ax25Error::MissingProtocolIdentifierExtension),
);
# Ok::<(), Ax25Error>(())
```

`UnknownUnnumberedKind` validates an unassigned U-frame modifier with its
poll/final bit cleared. `UnknownPid` is obtained by matching `Ax25Pid::Other`;
its byte is preserved. These variants retain unassigned wire values without
assigning them a protocol meaning.

## Frame validation

`Ax25Packet::new` rejects control/PID/information combinations that cannot be
encoded for the selected frame family. I and UI require a PID. Supervisory
frames forbid information; UI, FRMR, XID, TEST, and unknown U-frame modifiers
permit it. Other known U-frame kinds forbid it.

```rust
# extern crate alloc;
use alloc::vec::Vec;
use ax25_codec::{
    Ax25Control, Ax25Error, Ax25Header, Ax25Packet, Ax25Pid, CommandResponse,
    DigipeaterPath,
};

let header = Ax25Header {
    destination: "APRS".parse()?,
    source: "N0CALL-7".parse()?,
    digipeaters: DigipeaterPath::empty(),
    command_response: CommandResponse::Response,
};
let result = Ax25Packet::new(
    header,
    Ax25Control::from(0x01),
    Some(Ax25Pid::NoLayer3),
    Vec::new(),
);
assert!(matches!(
    result,
    Err(Ax25Error::UnexpectedProtocolIdentifier { control: 0x01 }),
));
# Ok::<(), Ax25Error>(())
```

## Frame check sequence

`ax25_fcs` computes the reflected CRC-16 with polynomial `0x1021`, initial
value `0xffff`, and final XOR `0xffff`. The resulting FCS is transmitted
least-significant octet first.

```rust
use ax25_codec::ax25_fcs;

let fcs = ax25_fcs(b"123456789");
assert_eq!(fcs, 0x906e);
assert_eq!(fcs.to_le_bytes(), [0x6e, 0x90]);
```

## Status

The public API is experimental and may change before release.

## Reference

AX.25 v2.2 specification, July 1998:
<https://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>.

Part of the [kenwood](https://github.com/swiftraccoon/kenwood) workspace.
License: GPL-2.0-or-later.
