# dstar-gateway-server

Multi-client D-STAR reflector server. All three protocols are dispatched at the
endpoint shell level. `wip/` is a scratch directory whose `.gitignore` excludes
everything in it; nothing there is part of the build.

## Running

`cargo run -p dstar-gateway-server` runs the `polaris` bin, a DExtra-only local
test reflector that explicitly disables DPlus and DCS. The LIBRARY default
(`ReflectorConfig::builder()`) enables all three, so do not read the bin's
configuration as the library default.

Overridable at runtime: `POLARIS_BIND` (default `0.0.0.0:30001`),
`POLARIS_CALLSIGN` (default `POLARIS`), `POLARIS_MODULES` (default `ABCD`, one
letter per module). `RUST_LOG` is read through `EnvFilter::try_from_default_env`.

## Contracts worth knowing

- `DExtra`/`DPlus` retransmit the cached header every 21 frames for late joiners. The boundary is `(seq_counter + 1)` being a multiple of 21, so the first rebroadcast happens after 20 data frames. `DCS` embeds headers in every voice/end packet; separate header conversion and periodic cache work emit no extra `DCS` packet (`xlxd/src/cdcsprotocol.cpp::HandleQueue`).
- The per-client TX token bucket defaults to 60 frames per second, overridable through the builder setter of the same name. Check this first when frames go missing.
- `ReflectorConfig` stores `enabled_protocols: HashSet<ProtocolKind>`, not individual bools, which keeps `struct_excessive_bools` quiet. Use `.enable(ProtocolKind::DPlus)` / `.disable(...)` on the builder and `config.is_enabled(ProtocolKind::DExtra)` to query.
- `ClientAuthorizer` gates LINK attempts; built-ins are `AllowAllAuthorizer`, `DenyAllAuthorizer` and `ReadOnlyAuthorizer`.
- `Reflector::run` consumes the reflector. Obtain its `ShutdownHandle` before starting it, signal shutdown, then await `run` to confirm every endpoint has finished. Dropping the future requests abort without joining destruction.
- Client records and endpoint settings are private. Custom drivers use `ProtocolEndpoint::from_config`, `EndpointOutcome::voice_route`, `clients_snapshot` and `client_count`; they retain the original datagram and source address.
- A module tracks one protocol/peer/stream tuple. Equal stream IDs from different peers or protocols cannot forward or clear another source's cache. Raw and transcoded forwarding share each recipient's token bucket.

## Tests

`tests/compile_fail/reflector_config_missing_callsign.rs` pins the builder's
required-callsign typestate. Re-bless its `.stderr` after any error-message
change:

```bash
TRYBUILD=overwrite cargo test -p dstar-gateway-server --test trybuild_compile_fail
```
