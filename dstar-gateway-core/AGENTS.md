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

- `rpt1` is the operator callsign (7 bytes padded) plus the local module letter at byte 7.
- `rpt2` is the reflector callsign (7 bytes padded) plus the reflector module letter at byte 7.
- xlxd checks `rpt1[7]` against its configured module count. At [revision `bf5d014`](https://github.com/LX3JL/xlxd/tree/bf5d0148dbdf2534af129ca3cc034c5051dcfc8d), `creflector.h:88` delegates to `creflector.cpp:559-566`: `0 <= module - 'A' < NB_OF_MODULES`. `main.h:64` sets that count to 10, admitting A-J, including G. `cdplusprotocol.cpp:209-250` logs a non-existing module. `Module` validates A-Z syntax; reflector configuration determines which letters are available.
- DCS LINK and UNLINK embed the target callsign at `[11..19]`; POLL uses `[9..17]`.
- `SessionBuilder::dcs_reflector(DcsReflector)` is required before a DCS client can build. Direct runtime configuration carries it in `ClientProtocol::Dcs`; no default target is substituted.

## Typestate sessions

`Session<P, S>` uses sealed protocol and state markers; transitions consume
`self`, and `Failed<S, E>` returns the original session on error.
`ServerSessionCore` checks server transitions at runtime. Prefer the infallible
fixture constructors in tests: `Module::A` through `Module::Z`, the const
`Callsign::from_wire_bytes` and `Suffix::from_wire_bytes`, and
`StreamId::new(n).unwrap()` in a `const` context, where a `None` fails compilation.

Trybuild cases in `tests/compile_fail/` pin the client typestate, and each has a
checked-in `.stderr`, so any
rustc or error-message change needs a re-bless:

```bash
TRYBUILD=overwrite cargo test -p dstar-gateway-core --test trybuild_compile_fail
```

## Fuzzing

Targets live in `fuzz/` and require nightly: `cargo +nightly fuzz list`, then
`cargo +nightly fuzz run <target>`.

## Host directories

`hosts::{HostName, HostAddress, HostEntry, HostFile}` validate and merge
Pi-Star-style host lists. Parsing is transactional and reports the original
line and typed failure; malformed ports or incomplete lines never fall back or
disappear. `HostName` is a catalog token, not a filesystem component. New
host-list work starts here; model/application layers retain their own directory
records and filesystem validation.
