# kiss-tnc

Generic KISS TNC wire framing using Chepponis and Karn (1987).
`#![no_std]` plus `alloc`, sans-io, zero async or I/O. Leaf crate: no workspace
path dependencies. The layering rule lives in the root `AGENTS.md` section
`Workspace boundaries`.

## Protocol contracts

- Keep radio-specific framing quirks outside this crate.
- Non-return command codes occupy `0x00..=0x06`. Return is the whole-byte
  `0xFF`; encoding it omits the frame record's port and payload.
- Reject malformed escape sequences for the whole frame. Preserve this strict
  decoding policy and document its difference from the specification.
- The decoder limit counts stuffed wire bytes and both delimiters. Enforce it
  during polling without dropping later valid frames in the same input chunk.
- Input ingestion retains the whole chunk. Do not describe the frame limit as
  a bound on queued bytes or retained allocation.
- Keep the closing delimiter after consuming a completed frame, including
  malformed and oversized frames, so shared delimiters remain supported.
- Preserve the dependency policy and add traits only when their semantics fit.

## Tests

Proptest strategies build validated newtypes with `prop_filter_map`, never
`unwrap`; for example `(0u8..=15).prop_filter_map("valid port", KissPort::new)`.
