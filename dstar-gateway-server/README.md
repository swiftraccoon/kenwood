# dstar-gateway-server

Multi-client D-STAR reflector server built on `dstar-gateway-core`.

Ships with all three D-STAR reflector protocols — `DExtra`
(XRF/XLX), `DPlus`, and `DCS` — each backed by a non-stub inbound
handler and enabled by default.

The server is a thin tokio shell over `dstar-gateway-core`'s
sans-io `ServerSessionCore`. It owns:

- A `Reflector` top-level type that binds UDP sockets and spawns
  one `ProtocolEndpoint<P>` task per enabled protocol.
- A per-endpoint `ClientPool<P>` with a reverse index from
  `Module` to `SocketAddr` for module-scoped fan-out.
- A pluggable `ClientAuthorizer` trait consulted on every inbound
  LINK attempt — the default `AllowAllAuthorizer` accepts every
  client with `AccessPolicy::ReadWrite`.
- A per-client TX token-bucket rate limiter and a
  `send_failure_count`-threshold unhealthy-client eviction path.
- A cross-protocol fan-out bus (broadcast channel +
  `transcode_voice` helper) gated behind the
  `cross_protocol_forwarding` config flag.

## `polaris` — local test reflector

The crate also ships a `polaris` binary: a minimal standalone
DExtra reflector that binds `0.0.0.0:30001` (override with the
`POLARIS_BIND` env var — e.g. `127.0.0.1:30001` to restrict it to
loopback) with callsign `POLARIS` and the default
`AllowAllAuthorizer`. It's the
counterpart we pair with the `sextant` GUI and `thd75-repl`'s
D-STAR mode for end-to-end voice-pipeline testing with no
external reflector in the loop:

```text
cargo run -p dstar-gateway-server --bin polaris
```

With POLARIS running, any DExtra client (sextant instance,
thd75-repl session, or a real radio configured to link to
`127.0.0.1 30001`) can join module `C` (or any of A-D — the
default set; extend via `POLARIS_MODULES`) and
exchange voice frames. A second client on the same module
hears the first; the reflector itself doesn't store or
transcode anything. See [`sextant/README.md`](../sextant/README.md)
for the typical two-client test flow.

## Examples

Run any of the shipped examples with
`cargo run -p dstar-gateway-server --example <name>`:

- `ref_reflector` — minimal `DExtra`-only reflector that binds
  `0.0.0.0:30001` with `AllowAllAuthorizer` and runs forever.
- `02_authorized_reflector` — custom `ClientAuthorizer` enforcing a
  banlist loaded from the `BANLIST_FILE` env var.
- `03_listen_only_dongles` — policy authorizer that grants callsigns
  starting with `D` `AccessPolicy::ReadOnly` and everyone else
  `ReadWrite`.
- `04_observability` — structured JSON logging
  with an env-driven `tracing-subscriber` filter.

Alpha. Not yet published to crates.io. Consume via path or git
dependency from the kenwood monorepo until the first published
release.
