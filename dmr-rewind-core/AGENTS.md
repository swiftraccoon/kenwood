# dmr-rewind-core (sans-io)

Codec for BrandMeister REWIND datagrams on the Open DMR Terminal service
(service `0x21`, UDP 54006): envelope, modeled DMR media bodies, call metadata
and the SHA-256 authentication digest. No socket, clock, retry, login flow or
session state; `dmr-rewind` is its tokio shell.

## Rules

- Open DMR Terminal is the only access mode. The Simple External Application service (UDP 54005, administrator-provisioned credentials) is deliberately unimplemented: unexpected control packet types decode as opaque bytes, and no API constructs that mode's configuration.
- This is an independent implementation. `go-brandmeister` (MIT) is the structural reference; `DigestPlay`'s `Rewind.h` and DJ4CK's `pyspot_rx` are GPL-3.0 cross-checks only, and no code from them may enter this GPL-2.0-or-later crate.
- The envelope is 18 little-endian bytes whose declared payload length must equal the rest of the datagram. A bad signature, truncation, trailing bytes, an oversized datagram or a wrong fixed body size is an error; an unknown packet type stays opaque.
- The two DMR IDs in a 12-byte full link control body are 24-bit big-endian (destination bytes 3-5, source 6-8), the exception to the little-endian wire. FLCO `0` is group voice and `3` private voice, and the raw control octet is preserved.
