# mmdvm-core

MMDVM frame and response codecs with owned decoded values. The crate has no
I/O, clock, runtime or logging dependency. The asynchronous transport and modem
session are provided by
[`mmdvm`](https://github.com/swiftraccoon/kenwood/tree/main/mmdvm).

## Scope

`MmdvmFrame` handles byte framing and preserves every command identifier.
`VersionResponse` checks supported protocol versions and firmware text;
`ModemStatus` reads protocol-specific status layouts. `Capabilities`,
`StatusFlags`, `ModemMode`, `HwType` and `NakReason` expose their interpreted
values. Command-specific payload requirements, modem configuration and transmit
policy belong to the caller. D-STAR is exercised by the workspace's modem
session; the other mode identifiers have no end-to-end modem qualification here.

## Encode and decode a frame

Constructed frames own at most 506 payload bytes. Known identifiers use typed
`Command` constants; `Command::from(byte)` preserves an unrecognized identifier.
Encoding uses the short header for payloads through 252 bytes and the extended
header for larger payloads. Decoding is incremental: an incomplete prefix
returns `None`; a complete frame reports its consumed byte count and leaves any
later bytes with the caller.

```rust
use mmdvm_core::{Command, MmdvmFrame};
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let frame = MmdvmFrame::with_payload(Command::DSTAR_DATA, vec![0xAA; 12])?;
let wire = frame.to_bytes();
assert_eq!(wire.get(..3), Some([0xE0, 15, 0x11].as_slice()));
assert!(MmdvmFrame::from_bytes(wire.get(..8).ok_or("prefix")?)?.is_none());

let mut received = wire;
received.extend_from_slice(&MmdvmFrame::new(Command::GET_STATUS).to_bytes());
let (decoded, consumed) = MmdvmFrame::from_bytes(&received)?.ok_or("incomplete frame")?;
assert_eq!(decoded, frame);
assert_eq!(received.get(consumed..), Some([0xE0, 3, 1].as_slice()));

let extended = MmdvmFrame::with_payload(Command::FM_DATA, vec![0x55; 300])?;
assert_eq!(extended.to_bytes().get(..4), Some([0xE0, 0, 49, 0x65].as_slice()));
assert_eq!(Command::from(0xFE).get(), 0xFE);
# Ok(())
# }
```

The short form is `[0xE0, total_length, command, payload...]`; its total length
is `3..=255`. In the extended form `[0xE0, 0, length2, command, payload...]`,
the total length is `length2 + 255`, or `255..=510`. Re-encoding chooses the
shortest form while preserving command and payload. An invalid start byte or a
short-form length of 1 or 2 returns a typed `MmdvmError`. Payloads exceeding 506
bytes are rejected during construction.

## Parse version and status responses

`ProtocolVersion` represents versions 1 and 2. Version 1 requires a protocol
byte; version 2 requires its complete twenty-byte fixed header. Text is checked
as UTF-8, trailing NULs and ASCII whitespace are removed, and embedded controls
are rejected. An empty description remains representable; an identity probe
supplies its own acceptance requirements. Version 2 retains both capability
bytes, including unknown bits. Version 1 has no capability bytes.

```rust
use mmdvm_core::{
    CAP1_DSTAR, CAP1_FM, CAP2_POCSAG, HwType, ModemMode, ModemStatus,
    ProtocolVersion, VersionResponse,
};
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let mut payload = vec![2, CAP1_DSTAR | CAP1_FM, CAP2_POCSAG, 0];
payload.extend_from_slice(&[0; 16]);
payload.extend_from_slice(b"MMDVM test\0 ");
let version = VersionResponse::parse(&payload)?;
assert_eq!(version.protocol(), ProtocolVersion::V2);
let capabilities = version.capabilities().ok_or("missing capabilities")?;
assert!(capabilities.has_dstar() && capabilities.has_fm());
assert!(capabilities.has_pocsag());
assert_eq!(HwType::from_description(version.description()), HwType::Mmdvm);

let status = ModemStatus::parse_v2(&[1, 0x41, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0])?;
assert_eq!(status.mode, ModemMode::Dstar);
assert!(status.tx() && status.cd());
assert_eq!(status.dstar_space, 12);
assert_eq!(format!("{:02X}", status.flags), "41");
assert!(ModemMode::try_from(0xFF).is_err());
assert_eq!(ModemMode::from_byte(0xFF), ModemMode::Idle);
# Ok(())
# }
```

Select `ModemStatus::parse_v1` or `parse_v2` from the negotiated version.
Version-1 status requires seven payload bytes and defaults its missing optional
P25, NXDN and POCSAG counts to zero. Version-2 status requires twelve bytes.
Shorter inputs return `MmdvmError::InvalidStatusLength`. Both status parsers use
the explicitly lenient `ModemMode::from_byte` fallback: unknown mode bytes become
idle. Use `ModemMode::try_from` on the raw mode byte when that distinction matters.
`StatusFlags::from_bits_truncate` retains the seven interpreted bits. The caller
decides how to report overflow and other status flags.

Ordinary mode identifiers are idle, D-STAR, DMR, YSF, P25, NXDN, POCSAG and FM.
Calibration state bytes and host application sentinels are outside that set.

## Inspect negative acknowledgements

NAK reason bytes are firmware and command dependent. The codec names verified
common cases and preserves other bytes in `NakReason::Unknown`. Code 5 can mean
insufficient TX queue capacity or rejection in the current state, so it does
not independently select a retry policy.

```rust
use mmdvm_core::{Command, MmdvmFrame, NakReason};
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let (frame, _) = MmdvmFrame::from_bytes(&[0xE0, 5, 0x7F, 0x11, 5])?
    .ok_or("incomplete NAK")?;
assert_eq!(frame.command(), Command::NAK);
let reason = NakReason::from(frame.payload().get(1).copied().ok_or("missing reason")?);
assert_eq!(reason, NakReason::QueueOrStateRejected);
assert_eq!(u8::from(NakReason::from(0xFE)), 0xFE);
# Ok(())
# }
```

## Reference and license

Frame lengths, identifiers and status/version offsets follow
[`MMDVMHost/Modem.cpp`](https://github.com/g4klx/MMDVM-Host/blob/be804801ac33f5b5d93278a2ad1507db0c269023/Modem.cpp).
The extended transmitter is `writeFMData`; the receiver is `getResponse`.
NAK layouts and state refusals follow
[`MMDVM/SerialPort.cpp`](https://github.com/g4klx/MMDVM/blob/57e692ad8ca380891b56a0988478ca76817e9f64/SerialPort.cpp),
with queue and data refusals in
[`MMDVM/DStarTX.cpp`](https://github.com/g4klx/MMDVM/blob/57e692ad8ca380891b56a0988478ca76817e9f64/DStarTX.cpp).
Ordinary mode values follow
[`MMDVM/Globals.h`](https://github.com/g4klx/MMDVM/blob/57e692ad8ca380891b56a0988478ca76817e9f64/Globals.h).

The crate is unpublished and its API is experimental. Portions are derived from
`MMDVMHost` by Jonathan Naylor G4KLX, 2015-2026, under GPL-2.0-or-later.
The existing project license is retained.
