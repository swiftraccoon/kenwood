# 0003: Shared codecs for client and server

Status: accepted.

## Decision

Client and server implementations share dstar-gateway-core's directional
codecs. They use distinct state APIs: Session<P, S> for consuming client
transitions and ServerSessionCore for checked server runtime transitions.
The server core retains linked identity/module and matches voice/EOT to its
active stream.

The reflector shell is a separate dstar-gateway-server package, depending
directly on the core. Client-only applications need no reflector dependency.
The unsupported server typestate facade is removed; ServerSessionCore is the
documented runtime entry point.

## Consequences

Shared encoding/decoding avoids maintaining two sets of wire constants.
Golden bytes and primary-source comparisons remain necessary: a round trip
alone cannot detect an encoder and decoder sharing the same mistake.
Client/server loopback tests cover interaction; server runtime regressions
cover identity and stream guards independently of client compile-fail tests.

Cross-protocol forwarding decodes into core header/frame values and reencodes
for the destination protocol. The shell owns authorization, active-stream
selection, recipient budgets and I/O results around that conversion.

## Alternatives

Separate per-side codec packages would duplicate layouts and constants.
A server wrapper that unconditionally changes its type after arbitrary input
would misrepresent the runtime state; retaining the checked runtime core
avoids that additional unsupported API.

## Implementation

- dstar-gateway-core/src/codec/: directional wire operations.
- dstar-gateway-core/src/session/server/: checked per-client runtime machine.
- dstar-gateway-server/src/: endpoints, reflector and forwarding shell.
