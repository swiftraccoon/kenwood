# dstar-gateway

D-STAR reflector gateway client library in Rust. Provides async UDP clients for the DExtra (XRF/XLX), DPlus (REF), and DCS reflector protocols behind a single unified `ReflectorClient` façade.

## Protocols

- **DExtra** (port 30001) — XRF and XLX reflectors
- **DPlus** (port 20001) — REF reflectors, with TCP authentication
- **DCS** (port 30051) — DCS reflectors

All three are fully implemented and covered by loopback integration tests.

## Architecture

```
[Radio MMDVM] <--your app--> [dstar-gateway] <--UDP--> [Reflector]
```

This crate handles the reflector (network) side. Your application provides the radio (MMDVM) side.

## Features

- Unified `ReflectorClient` enum for protocol-agnostic usage (`ReflectorClient::new(params)`)
- Unified `Error` enum — no `Box<dyn Error>` leaks in the public API
- Strong types throughout: `Callsign`, `Module`, `StreamId`, `Suffix` — validate once at the edge, pass by value
- `connect_and_wait(timeout)` convenience that drives the receive loop until the reflector acknowledges the link
- `set_poll_interval` for NAT-traversal tuning on DExtra/DCS keepalives
- DPlus auth host-list parsing (`auth_hosts()`) with a configurable auth endpoint for testing and private servers
- D-STAR header encode/decode with CRC-CCITT
- Pi-Star host file parser (2-column and 3-column formats)
- Voice frame types (AMBE + slow data)
- Automatic keepalives on the connected clients

## Quick start

Canonical connect → send voice → disconnect flow using the unified façade. This mirrors the crate-level doctest in `lib.rs`:

```rust,no_run
use dstar_gateway::{
    Callsign, DStarHeader, Module, Protocol, ReflectorClient,
    ReflectorClientParams, StreamId, Suffix, VoiceFrame,
};
use std::time::Duration;

# async fn example() -> Result<(), dstar_gateway::Error> {
let params = ReflectorClientParams {
    callsign: Callsign::try_from_str("W1AW")?,
    local_module: Module::try_from_char('B')?,
    reflector_callsign: Callsign::try_from_str("DCS001")?,
    reflector_module: Module::try_from_char('C')?,
    remote: "1.2.3.4:30051".parse().unwrap(),
    protocol: Protocol::Dcs,
};
let mut client = ReflectorClient::new(params).await?;
client.connect_and_wait(Duration::from_secs(5)).await?;

let header = DStarHeader {
    flag1: 0,
    flag2: 0,
    flag3: 0,
    rpt2: Callsign::try_from_str("DCS001 G")?,
    rpt1: Callsign::try_from_str("DCS001 C")?,
    ur_call: Callsign::try_from_str("CQCQCQ")?,
    my_call: Callsign::try_from_str("W1AW")?,
    my_suffix: Suffix::EMPTY,
};
let stream_id = StreamId::new(0x1234).unwrap();

client.send_header(&header, stream_id).await?;
let frame = VoiceFrame { ambe: [0; 9], slow_data: [0; 3] };
for seq in 0..5 {
    client.send_voice(stream_id, seq, &frame).await?;
}
client.send_eot(stream_id, 5).await?;
client.disconnect().await?;
# Ok(()) }
```

If you know the reflector callsign at runtime, use `Protocol::from_reflector_prefix("REF030")` to pick the right protocol automatically instead of hardcoding `Protocol::Dcs`.

## Per-protocol notes

### DExtra (XRF/XLX)

Supports cross-module linking: set `local_module` and `reflector_module` to different letters to route your local module B through the reflector's module C. The `thd75-repl` shorthand `link B:XRF030C` compiles to exactly this.

### DPlus (REF)

Must call `client.authenticate().await?` before `connect_and_wait`. Gate the call on `Protocol::requires_authentication(self)` if you're writing protocol-agnostic code — DExtra and DCS have no-op `authenticate()` implementations for API uniformity, so it's also safe to call unconditionally.

After successful authentication, `ReflectorClient::auth_hosts()` returns the parsed `HostList` from the auth server, which contains the current REF reflector endpoints. The auth endpoint defaults to `auth.dstargateway.org:20001` but can be overridden on the underlying `DPlusClient` via `set_auth_endpoint` — useful for integration tests and private auth servers.

### DCS

`DcsClient::send_header` caches the header internally. Subsequent calls to `send_voice` and `send_eot` pull the cached header automatically, which is why those methods don't take a header argument. Calling `send_voice` before `send_header` returns `Error::NoTxHeader`.

## Further reading

- `cargo doc --open -p dstar-gateway` — full API documentation with every public item documented
- `dstar-gateway/tests/` — runnable integration tests covering connect/voice/EOT/disconnect for each protocol, plus `dplus_auth.rs` for the auth flow and `fake_reflector.rs` for the loopback UDP harness you can reuse in your own tests

## References

Protocol formats derived from:
- g4klx/ircDDBGateway (GPL-2.0)
- LX3JL/xlxd (GPL-2.0)
- g4klx/MMDVMHost (GPL-2.0)

## License

GPL-2.0-or-later
