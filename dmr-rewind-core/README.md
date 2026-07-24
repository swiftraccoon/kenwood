# dmr-rewind-core

`dmr-rewind-core` is a sans-I/O codec for the BrandMeister REWIND UDP
protocol. It validates and emits complete datagrams, preserves packet types it
does not yet model, parses DMR call metadata, and provides the protocol's
authentication digest. It owns no socket, clock, retry loop, login flow, or
session state machine.

## Access mode

This crate supports BrandMeister Open DMR Terminal: service byte `0x21` on UDP
port `54006`, authenticated with the user's seven-digit DMR ID and that ID's
Hotspot Security password. It deliberately does not implement the separately
provisioned Simple External Application service.

The decoder still names unexpected REWIND control packet types and preserves
their bodies as opaque bytes. That defensive parsing does not expose another
login flow or let callers construct its mode-specific configuration.

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

Modeled media payloads use these exact sizes:

- DMR voice header full link control: 12 bytes
- DMR terminator: 0 bytes, or 12 bytes with full link control
- DMR audio burst: 27 bytes
- DMR embedded data: 10 bytes
- Super-header metadata: 32 bytes

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

## Protocol references and licensing

This crate independently implements public protocol behavior. The
MIT-licensed BrandMeister [`go-brandmeister` REWIND package][bm-go] was used
as the structural interoperability reference. Public wire constants and Open
Terminal behavior were also cross-checked against BrandMeister
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
