# aprs

## Boundaries

- This crate performs no I/O and reads no clock. Expiry and scheduling receive
  monotonic `Instant` values from a shell. Pure construction and matching need
  no time parameter.
- AX.25 identities and relay paths use canonical `ax25-codec` types. Preserve
  complete station addresses, including SSIDs, in matching and cache keys.
- `AprsFrameHeader` consumes caller-selected source, tocall and path. Mic-E
  derives its destination from the checked report and takes source/path
  separately. Generic builders never assume the TH-D75 software identity.
- Parsed observations are immutable. Derived comment, weather and extension
  views must agree; do not expose independent setters for cached views.

## Protocol and state rules

- `AprsData::from_bytes_with_destination` decodes Mic-E. The ordinary parser
  returns `MicERequiresDestination` because latitude occupies the destination.
- Mic-E speed/course occupy information bytes 4 through 6. Longitude rounded to
  180 degrees is unrepresentable in the degree table; reject it before encoding.
- Direct-message and bulletin destinations have different text grammar. Use
  their role-specific checked types; bulletin braces remain literal text.
- `PARM.` and `UNIT.` retain the chapter-13 68-byte body adaptation. Ordinary
  messages and bulletins use 67 bytes; NWS bodies are variable-width.
- Queue capacities are application policy. Never silently evict live message or
  relay state to admit more work. Station-list capacity has explicit eviction.
- Peeking work records no transmission. Commit after writing succeeds; retain
  work across cancellation. A returned digipeater relay is a protocol decision
  that the caller must retain until writing or explicit abandonment.
- Exhausted outbound messages wait one retry interval after their final attempt
  before expiry. Clock reversal preserves pending and duplicate-suppression state.
- DAO puts datum first. Preserve unknown datum bytes without assigning a scale.
  PHG's encoded height domain is wider than `u32` feet; narrowing is fallible.

## Checks

- Every integration test acknowledges unused dependencies with `use <dep> as _;`
  because each target is a separate compilation unit.
- The README is included in crate rustdoc. Keep Rust identifiers in backticks,
  use absolute repository links, and run doctests after workflow changes.
- `cargo nextest run -p aprs --test campaign_reference --no-capture` prints the
  pinned reference payloads; it performs no radio or server I/O.
