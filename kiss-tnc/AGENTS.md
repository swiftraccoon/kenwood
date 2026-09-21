# kiss-tnc

Generic KISS TNC wire framing, spec-correct per Chepponis and Karn (1987).
`#![no_std]` plus `alloc`, sans-io, zero async or I/O. Leaf crate: no workspace
path dependencies. The layering rule lives in the root `AGENTS.md` section
`Workspace boundaries`.

## D75-specific quirks are NOT here

- The TH-D75 firmware reportedly nibble-splits `CMD_RETURN` where the spec says it should not. This crate stays spec-correct so the TM-D750 and other TNCs can share it.
- That quirk is currently unimplemented anywhere, not implemented elsewhere: the KISS session site is `thd75/src/radio/kiss_session.rs`, and its `exit()` sends this crate's `KissFrame::return_command()`, the spec-correct whole byte `0xFF`. Re-verify against hardware KISS before relying on the quirk claim at all.
- The invariant a nibble split would break is in `src/frame.rs`: a non-Return command's wire byte is always `0x00..=0x06`, so the port (high nibble) and command (low nibble) never overlap, while `Return` is the whole byte `0xFF` and carries neither port nor payload.

## Tests

Proptest strategies build validated newtypes with `prop_filter_map`, never
`unwrap`; for example `(0u8..=15).prop_filter_map("valid port", KissPort::new)`.
