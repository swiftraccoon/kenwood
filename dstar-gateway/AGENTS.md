# dstar-gateway (tokio shell)

Async wrapper over `dstar-gateway-core`. This layer is the clock source for the
sans-io core, so `Instant::now()` is allowed here and nowhere below it.

This crate owns the workspace's only mdBook (`book/`); `lint.sh` builds it with
`mdbook build dstar-gateway/book`.

## Shared entry points (do not hand-roll replacements)

- `tokio_shell::drive_connecting(session, socket, deadline)` pumps a `Session<P, Connecting>` to `Connected` with typed `ConnectError`. All reflector clients delegate here; per-protocol preludes such as DPlus auth stay caller-side.
- `tokio_shell::fresh_stream_id()` is the clock-seeded TX stream-ID source and maps the reserved zero.
- `tokio_shell::AnyAsyncSession` and `AnyEvent` are the protocol-erased session handle and event, and are lossless because the core `Event<P>` is phantom-generic. Consumers alias their session types onto these instead of hand-rolling three-armed enums.
- TX order is `AsyncSession::spawn(connected, socket)`, then `send_header(hdr, sid)`, then repeated `send_voice(sid, seq, frame)`, then `send_eot(sid, seq)`, then `disconnect()` (which takes no timeout argument).

## Session-loop silent death

- `SessionLoop::run()` is instrumented: every exit path logs.
- Client `handle_*_input` methods swallow decoder errors as `tracing::debug` diagnostics and **never** propagate them with `?`.
- A `handle_input` error in the session loop kills the entire session, so the fix always belongs at the decode boundary, not in the loop.
- Log greps: `mio::poll: deregistering` after a VoiceEnd means the session loop died; `session loop exited` shows clean versus error exit; `rejected datagram` is lenient decode dropping an unrecognized packet, which is normal for reflectors.
- The DPlus auth host resolves to several addresses and not all of them answer, so `AuthClient` tries each with a 3 s per-address timeout (happy eyeballs).

## Features and testing

- `insecure-plaintext-xlx-directory` is an explicit reqwest opt-in to the XLX registry's plaintext-HTTP-only endpoint; responses have no authenticity or integrity. `hardware-tests` and `examples-network` gate live-network code, and `blocking` adds the sync shell.
- `tests/conformance.rs` is `#[ignore]`d and replays pcaps. The corpus under `tests/conformance/corpus/<proto>/` is deliberately not committed; drop local `.pcap` files there and run with `--ignored`.
- Compile-fail expectations re-bless with `TRYBUILD=overwrite cargo test -p dstar-gateway --test trybuild_compile_fail`. Fuzz targets live in the core crate.
- Examples `04` through `10` carry `required-features` (`examples-network`, plus `blocking` for the blocking CLI), which is why a plain `cargo build --examples` silently skips most of them.

## Hardware tests

Triple-gated by the `hardware-tests` feature, `--ignored`, and env vars. Each
`DSTAR_TEST_REFLECTOR_*` value is a host NAME and the test appends the port
(`:20001` DPlus, `:30001` DExtra, `:30051` DCS). `DSTAR_TEST_REFLECTOR_CALLSIGN`
is the routing callsign in the TX header, kept separate from the DNS hostname in
`DSTAR_TEST_REFLECTOR_DPLUS`. The voice-burst TX test additionally requires
`DSTAR_TEST_TX_OK=1`; without it the test returns early and keys nothing.

```bash
DSTAR_TEST_CALLSIGN=<CALL> \
DSTAR_TEST_REFLECTOR_DPLUS=REF030 \
DSTAR_TEST_REFLECTOR_DEXTRA=XLX307 \
DSTAR_TEST_REFLECTOR_DCS=DCS001 \
DSTAR_TEST_REFLECTOR_CALLSIGN=REF030 \
cargo test -p dstar-gateway --features hardware-tests --test hardware -- --test-threads=1 --ignored
```

## Reference clones

`REFERENCES.md` cites `g4klx/ircDDBGateway` and `LX3JL/xlxd` by file and line
against pinned commits. Clone them into the gitignored reference directory so
those citations resolve locally:

```bash
git clone https://github.com/g4klx/ircDDBGateway ref/ircDDBGateway
git clone https://github.com/LX3JL/xlxd ref/xlxd
```
