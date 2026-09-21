# dstar-gateway-core (sans-io)

Runtime-agnostic, I/O-free D-STAR core.

- **The core must never call `Instant::now()`**; always use the injected `now` parameter. The tokio shell (`dstar-gateway`) is the clock source.
- The modem-side echo policy, with its own frame cap, lives in `mmdvm::dstar`, not here. This crate's `echo` module is the sans-io record-and-replay state machine only.
- Reference sources are ircDDBGateway and xlxd, cited de-pathed per the root `AGENTS.md` section `Reference hierarchy`. They are truth for D-STAR **reflector** protocols only, never for TH-D75 CAT or MCP.

## Lenient parsing (do not tighten)

- All `decode_*` functions take `&mut dyn DiagnosticSink`.
- Unknown-length datagrams are swallowed as debug diagnostics in `handle_*_input`. They are **never** propagated with `?`, because that kills the session loop.
- REF030 sends a 10-byte packet after every voice EOT, so the decoder must tolerate unknown lengths.

## Header rpt1/rpt2 convention (verified against xlxd and ircDDBGateway)

- `rpt1` is the operator callsign (7 bytes padded) plus the LOCAL module letter at byte 7 (A-E, never `G`).
- `rpt2` is the reflector callsign (7 bytes padded) plus the REFLECTOR module letter at byte 7 (A-E, never `G`).
- xlxd reads `rpt1[7]` as the module and `IsValidModule('G')` is false, which means a silent packet drop.
- DCS LINK, UNLINK and POLL packets embed the reflector callsign at wire offset `[11..19]`; it must match the actual target.
- `SessionBuilder::reflector_callsign(cs)` sets that callsign. It is optional in the builder but REQUIRED for DCS: unset it defaults to `DCS001`, which is wrong for every other DCS reflector.

## Typestate sessions

`Session<P, S>` uses sealed protocol and state markers; transitions consume
`self`, and `Failed<S, E>` returns the original session on error.
`ServerSessionCore` carries the mirror server typestate. Prefer the infallible
fixture constructors in tests: `Module::A` through `Module::Z`, the const
`Callsign::from_wire_bytes` and `Suffix::from_wire_bytes`, and
`StreamId::new(n).unwrap()` in a `const` context, where a `None` fails compilation.

Trybuild cases in `tests/compile_fail/` pin both the client typestate and the
server LINK/UNLINK/voice ordering, and each has a checked-in `.stderr`, so any
rustc or error-message change needs a re-bless:

```bash
TRYBUILD=overwrite cargo test -p dstar-gateway-core --test trybuild_compile_fail
```

## Fuzzing

Targets live in `fuzz/` and require nightly: `cargo +nightly fuzz list`, then
`cargo +nightly fuzz run <target>`.

## Host directories

`hosts::{HostEntry, HostFile}` plus `parse_hosts_text` and `merge_directories`
parse and merge Pi-Star-style host lists. lodestar-core uses them; sextant's
`hosts.rs` is a deliberately separate hand-rolled implementation. New host-list
work starts here, not in a new parser.
