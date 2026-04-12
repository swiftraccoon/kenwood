# dstar-gateway-server

Multi-client D-STAR reflector server built on `dstar-gateway-core`.

Currently ships with `DExtra` (XRF/XLX) endpoint support. `DPlus`
and `DCS` server endpoints are open work — see the parent
`dstar-gateway` README for the current deferred-work list.

The server is a thin tokio shell over `dstar-gateway-core`'s
sans-io `ServerSessionCore<P>`. It owns:

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

Alpha. Not yet published to crates.io. Consume via path or git
dependency from the kenwood monorepo until the first numbered
release.
