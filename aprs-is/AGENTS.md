# aprs-is

Tokio shell over `aprs`. This layer calls `Instant::now()` freely and IS the
clock source for the sans-io core below it. The DAG rule is in the root
`AGENTS.md` section `Workspace boundaries`.

```bash
cargo run -p aprs-is --example monitor -- <CALL>   # receive-only APRS-IS network witness
```

The `examples/monitor.rs` name collides with the thd75 example of the same name
in the shared target directory: always select the package explicitly. The
on-air validation phase runner that pairs with this witness is listed in
`thd75-repl/AGENTS.md`.

## Rules

- `ax25-codec` is a direct dependency because `Ax25Address`, `RouteEntry` and `Ax25Packet` appear in the public IGate and q-construct signatures. They are NOT re-exported through `aprs`, so do not try to reach them that way.
- The default server and port are set in `AprsIsConfig::new`, not in `client.rs`.
- `AprsIsFilter` implements a subset of the APRS-IS filter grammar (`raw`, `negated`, `to_wire`, `join`); check the public grammar before assuming a filter form is supported. Structured payloads and combination limits are checked; raw extensions check framing only.
- Reconnection is explicit. `connect_with_retry()` retries transport failures with a nonzero limit or indefinitely; `reconnect()` preserves the old client until replacement succeeds.
- A packet upload requires `LoginState::Verified`, established by the matching server login response. `Passcode::Verified` supplies credentials only. Use `IgateMessaging::Bidirectional` for `qAR` and `IgateMessaging::ReceiveOnly` for `qAO`; both require verified Internet login.
