# pulsar

Receive-only BrandMeister DMR call recorder over `dmr-rewind`: one Open
Terminal stream per `[[master]]`, one raw AMBE+2 container plus JSON sidecar
per call.

```bash
cargo run -p pulsar -- check --config pulsar.toml   # validate the file and password variables; no connection
cargo run -p pulsar -- --config pulsar.toml         # record until Ctrl-C
```

`pulsar.toml` and `dmr-recordings/` are gitignored; `pulsar.example.toml` is the
tracked starter.

## Invariants

- Receive-only: Pulsar never sends DMR voice or presents itself as a hotspot or repeater.
- No audio decode and no WAV. DMR AMBE+2 is not the D-STAR AMBE 3600x2400 codec, and `mbelib-rs` deliberately does not implement AMBE+2; never route these frames to its decoder.
- Write order: reserve the whole stem with `create_new`, install synced raw `.ambe` first and synced JSON last through non-overwriting hard links. Sync the hierarchy and ancestor directory entries before installation and the containing directory after each install on Unix. A recording exists iff its JSON exists, including an error after JSON installation; never overwrite that pair on retry. Any persistence failure stops the process rather than dropping later calls.
- Completed calls pass through a bounded writer queue, so slow storage applies backpressure instead of growing memory.
- `max_capture_records_per_call` is one budget shared by audio, voice-header and embedded link-control records. A call that exceeds it is discarded whole and stops Pulsar with an error; never write a truncated recording.
- The container is a reader contract: magic `PLSRAMBE`, version 1, 34-byte little-endian records (`sequence u32`, `flags u16`, `subtype u8`, the 27 AMBE+2 bytes unchanged). A layout change changes the version.
- Hotspot Security passwords come only from the environment variable that `password_env` names; never put one in TOML, a log line or a fixture.
