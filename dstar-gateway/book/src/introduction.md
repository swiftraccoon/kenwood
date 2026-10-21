# Introduction

This book introduces the DPlus, DExtra and DCS reflector stack. The
[API reference](https://swiftraccoon.github.io/kenwood/dstar_gateway/)
and
[architecture](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/ARCHITECTURE.md)
describe its detailed contracts.

- dstar-gateway-core owns directional codecs, checked transmit domains,
  lossless receive fields and injected-time client/server state machines.
- dstar-gateway owns Tokio and synchronous client drivers, bounded TCP
  authentication and optional untrusted HTTP directory retrieval.
- dstar-gateway-server owns reflector authorization, client pools,
  per-recipient forwarding policy and endpoint tasks.

Client consuming transitions use typestate; runtime driver activity can
subsequently close a session, so sends also check runtime state.
Server transitions use ServerSessionCore's checked runtime API.
Both directions share the codecs.

Decoders return typed framing errors and record their implemented recoverable
diagnostics. The caller can inspect raw receive values and observations before
applying its own acceptance policy. Diagnostics are not a certificate that
every received field was validated.

Start with [protocol boundaries](getting-started/what-is-dstar.md),
[installation](getting-started/installation.md), then
[Hello, local DPlus](getting-started/hello-dplus.md). That walkthrough includes
the maintained Cargo example directly so its listing shares compilation checks.
The
[examples](https://github.com/swiftraccoon/kenwood/tree/main/dstar-gateway/examples)
cover the other protocols, TX, DPRS, controlled bridging, synchronous driving
and raw recording.

The
[reference index](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/REFERENCES.md)
pins public protocol sources. Model and modem crates document radio/interface
contracts separately.
