# dmr-rewind (tokio shell)

Receive-only Tokio client for the BrandMeister REWIND Open DMR Terminal service
over `dmr-rewind-core`, and the clock source for its session. It never
transmits DMR, decodes AMBE+2, chooses a master, resolves DNS or reconnects:
those are the supervising application's policy (`pulsar`).

## Rules

- `ClientConfig` defaults to a 5 s keepalive and a 15 s valid-packet timeout, and the timeout must exceed the keepalive. Subscriptions are installed one at a time, each acknowledged before the next; a fresh challenge on an established session re-authenticates and reinstalls them.
- A timeout or socket failure is returned, never retried: re-resolution, address choice and backoff belong to the caller.
- One Open Terminal connection carries at most one active call; its subscriptions filter that one slot and are not concurrent tuners.
- Credential fields have redacted `Debug` implementations; never log the password or the variable it came from. The UDP socket is connected to the master, so the OS drops datagrams from other sources.
- Events keep the original datagram, receipt time, transport sequence and every flag bit; never normalize them away.
- `tests/open_terminal.rs` drives a fake master on `127.0.0.1`; no test reaches BrandMeister.
