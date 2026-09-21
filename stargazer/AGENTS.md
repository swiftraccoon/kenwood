# stargazer

D-STAR reflector voice recorder: config-listed targets, listen-only sessions,
three files per transmission (`.ambe`, `.wav`, `.json`).

```bash
cargo run -p stargazer -- --config stargazer.toml            # record until Ctrl-C
cargo run -p stargazer -- survey                             # activity poller (archive-first)
cargo run -p stargazer -- report --window-hours 24           # rank reflector modules from the archive
cargo run -p stargazer -- harvest --target REF030-C          # fetch published recordings
cargo run -p stargazer -- import-dvrec                       # rebuild recordings from salvaged dvrec logs
cargo run -p stargazer -- ctl status|disable <T>|enable <T>|reload
```

## Invariants

- **Write order is `.ambe`, `.wav`, `.json`**; the JSON is fsynced and renamed last, so a recording exists iff its `.json` exists. An `.ambe` failure aborts the recording; a `.wav` failure is recorded in `audio.error` and tolerated.
- Container: magic `STGZAMBE`, version 1, 13-byte records (`seq u8 | ambe [9] | slow_data [3]`), little-endian, arrival order.
- One `AmbeDecoder` per stream; adaptive decoder state must never cross talkers. Gaps are filled with `conceal_frame()` so PCM length equals codec time.
- `capture.rs` is pure and clock-injected: it never reads the clock or the filesystem. The session shell passes `now` and the writer does I/O on `spawn_blocking`.
- The slow-data assembler skips seq 0; sync bytes corrupt that block.
- `local_module` is restricted to A-E because xlxd drops the others.
- Disables persist in `recordings/.disabled-targets` across restarts, which frees a reflector for sextant testing without the recorder stealing it back: DPlus displaces a same-callsign login on ANY module.

## Harvest politeness (hard requirement, volunteer servers)

- robots.txt gate before anything else, one sequential connection, a 2 s gap between downloads, a user agent naming the tool and operator, abort the run after 3 consecutive failures, and never re-download on a re-run. Do not weaken any of these without the operator's say-so.
- The activity poller's robots.txt is ambiguous, so treat the monitoring endpoint as a service rather than crawl surface; the 30 s minimum poll interval is enforced in code, stay gentle.
- Matching key for published recordings is (sanitized callsign, stream id) with 120 s tolerance, nearest time, read from `.json` sidecars and never from filename stems.
- The published `.mp3` is a server-side decode, not mbelib, and is better synthesis than ours. The published packet log matches our capture byte for byte, and our decoder is deterministic (a fresh decode equals the recorded wav). Retention is short, so harvest same-day.
- In dvrec packet logs, `vd :<18 hex>` is 9 AMBE wire bytes and the header line is `flags:RPT2:RPT1:UR:MY/SUFX`. **The per-line stream id is byte-swapped relative to the filename stream id.**
- Twins matched on (sid, 120 s) are never overwritten, and gap-fill dvrecs stay unimported. Run order is harvest, then import-dvrec, then align.
- The survey poller stores raw bytes before parsing; events dedupe on (ts, gateway, mycall, rpt1) and `gap_risk` flags a window whose rows are all newer than the previous poll's newest row.
- `import-dvrec`'s exit code reflects only tree-walk I/O errors: per-file failures are warned and counted in the summary line, so the exit code never signals a format drift. Watch the summary line: `imported 0` for consecutive days, or a spike in `failed`, is the only signal that the published format drifted.

## Debugging

- `tests/loopback.rs` is the entry point for the record path: a fake DExtra reflector pushes LINK-ACK, header, frames and EOT, and asserts the three files. Run with `RUST_LOG=debug cargo test -p stargazer --test loopback -- --nocapture`.
- `tests/harvest_http.rs` is the entry point for the fetch path: a canned local HTTP server backs dry-run planning, a real run with provenance, an idempotent re-run, the robots gate, the download limit and the failure breaker. No live server is ever contacted.
- All-zero AMBE frames are NOT FEC-clean (descrambled C1 needs 2 corrections); the real zero-error baseline is the DVSI silence frame, so do not "fix" the FEC tests that pin this.
- Smoke test against nothing: point a target at `127.0.0.1:39999` and expect connect-failure warnings with backoff and no panics.
