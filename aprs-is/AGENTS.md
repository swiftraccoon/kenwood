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
- `AprsIsFilter` implements a subset of the APRS-IS filter grammar (`raw`, `negated`, `as_wire`, `join`); the real grammar is wider, so check before assuming a filter form is supported.
- There is no automatic retry loop. `AprsIsClient::reconnect()` re-dials and re-logins; callers own their backoff policy.
