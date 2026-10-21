# kiss-tnc

KISS TNC wire-framing codec with validated ports, typed commands, one-shot frame
conversion and streaming reassembly. The crate uses `no_std` and `alloc`; the
application supplies an allocator and transport I/O.

## Scope

[`KissFrame`] owns a command's payload bytes. [`KissPort`] validates the four-bit
port address, and [`KissCommand`] identifies the assigned command codes. Command
payloads remain opaque: callers validate AX.25 data and command parameters at the
appropriate protocol layer. [`KissDecoder`] retains input chunks and extracts
complete frames without reading the clock or performing I/O.

The [KISS specification](https://www.ka9q.net/papers/kiss.html) defines the wire
bytes and command assignments. This codec rejects malformed escape sequences
with an error for the whole frame; the specification describes continuing frame
assembly after an invalid escaped byte. Empty delimiter pairs are skipped by the
streaming decoder and reported as [`KissError::EmptyFrame`] by one-shot decoding.

## Ports and commands

Port addresses range from zero through fifteen. Command conversion accepts the
assigned low-nibble codes and the whole-byte [`KissCommand::Return`] code.
The inherent scalar conversion functions also support constant expressions.

```rust
use kiss_tnc::{CMD_DATA, KissCommand, KissPort};

let port = KissPort::try_from(12_u8)?;
let command = KissCommand::try_from(CMD_DATA)?;
let type_byte = (port.get() << 4) | command.as_byte();
assert_eq!(KissPort::from_type_byte(type_byte), port);
assert_eq!(u8::from(port), 12);
assert_eq!(u8::from(command), CMD_DATA);
assert_eq!(KissCommand::from_byte(CMD_DATA), Some(command));
assert_eq!(KissPort::new(16), None);
assert_eq!(KissPort::default(), KissPort::ZERO);
# Ok::<(), kiss_tnc::KissError>(())
```

## Encoding and decoding

[`KissFrame::to_bytes`] allocates a wire buffer. [`KissFrame::append_to`] appends
to reusable caller-owned storage and preserves bytes already present. Both
methods stuff the type byte and payload between [`FEND`] delimiters.
[`KissFrame::from_bytes`] accepts one complete frame, including any extra leading
FEND fill, and returns an owned payload.

```rust
use kiss_tnc::{FEND, FESC, KissCommand, KissFrame, KissPort};

let frame = KissFrame {
    port: KissPort::try_from(12_u8)?,
    command: KissCommand::Data,
    data: vec![FEND, FESC, 0x41],
};
let wire = frame.to_bytes();
assert_eq!(KissFrame::from_bytes(&wire)?, frame);

let mut output = vec![0xAA];
frame.append_to(&mut output);
assert_eq!(output.first().copied(), Some(0xAA));
assert_eq!(output.get(1..), Some(wire.as_slice()));
assert_eq!(KissFrame::data(vec![0x41]).port, KissPort::ZERO);
# Ok::<(), kiss_tnc::KissError>(())
```

## Return command

The return command occupies the entire type byte, `0xFF`. Encoding it emits
exactly `FEND 0xFF FEND`, omitting the record's port and payload. Decoding it
sets the port to zero and retains any received payload bytes in the record.

```rust
use kiss_tnc::{CMD_RETURN, FEND, KissFrame};

let frame = KissFrame::return_command();
assert!(frame.command.is_return(), "return frame carries the return command");
assert_eq!(frame.to_bytes(), vec![FEND, CMD_RETURN, FEND]);
assert_eq!(KissFrame::from_bytes(&frame.to_bytes())?, frame);
# Ok::<(), kiss_tnc::KissError>(())
```

## Streaming

Push arbitrary chunks, then call [`KissDecoder::next_frame`] until it returns
`Ok(None)`. A single FEND can close one frame and open the next. Escape pairs
may cross chunk boundaries.

```rust
use kiss_tnc::{FEND, FESC, KissDecoder, TFEND};

let mut decoder = KissDecoder::new();
decoder.push(&[FEND, 0x00, FESC]);
assert_eq!(decoder.next_frame()?, None);
decoder.push(&[TFEND, FEND, 0x00, 0x41, FEND]);

let mut payloads = Vec::new();
while let Some(frame) = decoder.next_frame()? {
    payloads.push(frame.data);
}
assert_eq!(payloads, vec![vec![FEND], vec![0x41]]);
# Ok::<(), kiss_tnc::KissError>(())
```

## Limits and errors

[`DEFAULT_MAX_FRAME_LEN`] is 1024 wire bytes. A custom limit counts both FEND
delimiters, the stuffed type byte and stuffed payload. [`KissDecoder::push`]
copies its entire input. The limit is checked by `next_frame` and applies to
each frame; several queued frames may together exceed it. Buffer capacity is
retained after decoding or discarding bytes. Callers control queued input by
choosing chunk sizes and draining frames between pushes.

An oversized completed frame is consumed through its body, preserving its
closing FEND for the next frame. Oversized incomplete input or a buffer without
a FEND is cleared, and subsequent input is scanned for a new opening FEND.
Malformed completed frames are consumed in the same way as completed oversized
frames, so callers can continue after an error. Limits below three reject every
complete frame.

```rust
use kiss_tnc::{FEND, FESC, KissDecoder, KissError, TFEND};

let mut decoder = KissDecoder::with_max_frame_len(4);
decoder.push(&[FEND, 0x00, FESC, TFEND, FEND, 0x00, 0x41, FEND]);
let result = decoder.next_frame();
assert!(
    matches!(result, Err(KissError::FrameTooLong)),
    "five wire bytes exceed the four-byte limit: {result:?}",
);

let mut payloads = Vec::new();
while let Some(frame) = decoder.next_frame()? {
    payloads.push(frame.data);
}
assert_eq!(payloads, vec![vec![0x41]]);
# Ok::<(), kiss_tnc::KissError>(())
```

## Status

Pre-release. The public API can change; pin a git revision for reproducible
builds.

## Reference

KISS protocol: Chepponis and Karn (1987),
<https://www.ka9q.net/papers/kiss.html>.

Part of the [kenwood workspace](https://github.com/swiftraccoon/kenwood).
License: GPL-2.0-or-later.
