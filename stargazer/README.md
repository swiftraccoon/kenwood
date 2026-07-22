# stargazer

D-STAR reflector voice recorder. A TOML config lists the reflectors you
want to record; stargazer connects to each as a listen-only client and
writes every received transmission to disk as three files: the raw AMBE
frames (ground truth), a decoded 8 kHz WAV (for listening and review),
and a metadata JSON (callsigns, timing, signal quality, GPS).

## Quick start

Create `stargazer.toml`:

```toml
callsign = "N0CALL"            # your callsign: reflector login + DPlus auth
recordings_dir = "recordings"  # created if missing
write_wav = true               # decoded WAV alongside the raw AMBE
local_module = "D"             # module letter presented in rpt1 (A-E)

[[record]]
reflector = "REF030"
protocol = "dplus"             # dplus | dextra | dcs
host = "ref030.dstargateway.org"
modules = ["C"]                # one session per listed module

[[record]]
reflector = "XLX039"
protocol = "dextra"
host = "xlx039.example.org"
port = 30001                   # optional; defaults: dplus 20001, dextra 30001, dcs 30051
modules = ["A", "B"]
```

Then:

```bash
cargo run -p stargazer -- --config stargazer.toml
```

Stargazer connects to every `(reflector, module)` pair, records until
you press Ctrl-C, and heals dropped links automatically with jittered
exponential backoff. `--verbose` enables per-frame debug logging.

## Configuration reference

| Field | Default | Meaning |
|---|---|---|
| `callsign` | *required* | Operator callsign used for reflector login and `DPlus` auth |
| `recordings_dir` | `"recordings"` | Base directory for recordings (created if missing) |
| `write_wav` | `true` | Decode and write a WAV next to each raw AMBE archive |
| `local_module` | `"D"` | Local module letter in rpt1 (A–E; other letters are silently dropped by xlxd-derived reflectors) |
| `[[record]].reflector` | *required* | Reflector callsign, e.g. `"REF030"` (also the wire callsign for DCS) |
| `[[record]].protocol` | *required* | `dplus`, `dextra`, or `dcs` |
| `[[record]].host` | *required* | Host name or IP; always explicit, there is no discovery |
| `[[record]].port` | per protocol | `dplus` 20001, `dextra` 30001, `dcs` 30051 |
| `[[record]].modules` | *required* | Module letters (A–Z); each gets its own session |

`DPlus` targets attempt authentication against the D-STAR gateway auth
service first; if that fails, stargazer logs a warning and connects
unauthenticated (most reflectors accept the link anyway).

## Recording layout

```
recordings/
  REF030-C/                       # <reflector>-<module>
    2026-07-09/                   # UTC date of stream start
      20260709T183042Z_W1AW_04D2.ambe
      20260709T183042Z_W1AW_04D2.wav
      20260709T183042Z_W1AW_04D2.json
```

Filenames are `<UTC start time>_<callsign>_<stream id hex>`. Callsigns
are sanitized to `[A-Z0-9-]`; a transmission whose header was never
received records as `UNKNOWN`. Name collisions get a `-1`, `-2`, …
suffix.

**A recording exists iff its `.json` exists.** Files are written to a
temp name and renamed into place (`.ambe`, then `.wav`, then the
fsynced `.json` last), so a crash can leave stray `*.tmp` files but
never a torn recording.

## File formats

### `.ambe`: raw frame container

Everything the reflector sent for the stream, exactly as it arrived.
All integers little-endian.

```
Header (16 bytes):
  magic       [u8; 8] = "STGZAMBE"
  version     u16     = 1
  record_len  u16     = 13
  reserved    u32     = 0
Records (13 bytes each, arrival order):
  seq         u8       D-STAR frame sequence (0..=20)
  ambe        [u8; 9]  AMBE 3600x2400 voice bytes
  slow_data   [u8; 3]  raw slow-data bytes
```

This file is the canonical archive: the decoded WAV, per-frame FEC
error counts, codec parameters for vocoder-domain ASR, and any future
(better) decode are all deterministic re-derivations from it. It costs
about 650 bytes per second of speech, roughly 2.3 MB per hour of
continuous talk.

### `.wav`

8000 Hz, mono, 16-bit PCM. Frames lost on the RF uplink (sequence
gaps) are filled with the decoder's concealment output so the audio
timeline matches the transmission's codec time.

### `.json`: metadata (schema `stargazer-recording/1`)

```json
{
  "schema": "stargazer-recording/1",
  "writer": "stargazer/0.2.0",
  "reflector": "REF030",
  "module": "C",
  "protocol": "dplus",
  "host": "ref030.dstargateway.org:20001",
  "peer": "203.0.113.7:20001",
  "stream_id": "04D2",
  "started_at": "2026-07-09T18:30:42.113Z",
  "ended_at": "2026-07-09T18:31:07.554Z",
  "duration_s": 25.44,
  "end_reason": "eot",
  "header": {
    "my_callsign": "W1AW",
    "my_suffix": "D75",
    "ur_callsign": "CQCQCQ",
    "rpt1": "REF030 C",
    "rpt2": "REF030 G",
    "flags": [0, 0, 0],
    "raw_hex": "…the parsed header re-encoded to its 41-byte wire form…",
    "diagnostics": []
  },
  "frames": { "received": 1272, "expected": 1272, "gaps": 0 },
  "fec": {
    "corrected_bits": 3,
    "frames_with_errors": 2,
    "erasure_frames": 0,
    "tone_frames": 0
  },
  "slow_data": {
    "text": "Asheville NC",
    "dprs": [
      { "callsign": "W1AW", "lat": 35.5951, "lon": -82.5515,
        "symbol": "[", "comment": "walking", "at_frame": 512 }
    ]
  },
  "audio": { "wav": true, "concealed_frames": 0 }
}
```

Field notes:

- `duration_s` is codec time (`expected` frames × 20 ms); the
  wall-clock delta additionally includes network jitter.
- `end_reason` is `eot` (real end-of-transmission), `inactivity`
  (stream timed out), `disconnect` (link dropped mid-stream), or
  `shutdown` (stargazer exited mid-stream). Partial captures are
  first-class recordings with honest metadata, not discarded.
- `header` is `null` when no voice header was ever received.
- `frames.gaps` counts frames missing from the sequence; `fec` sums
  Golay/ECC corrections across the stream; both are quality indexes
  whose ground truth remains the `.ambe` file.
- `slow_data.text` is the radio's 20-character TX message; when the
  message is not clean printable ASCII (Japanese radios commonly send
  JIS X 0201 half-width katakana), `slow_data.text_hex` additionally
  carries the raw 20 bytes losslessly. `dprs` lists every parsed DPRS
  position with the frame index where it completed.

## Activity survey

To decide *which* reflectors are worth recording, stargazer can survey
network-wide DPLUS voice activity:

```bash
cargo run -p stargazer -- survey             # poll + archive every 60 s (Ctrl-C to stop)
cargo run -p stargazer -- survey --once      # single poll
cargo run -p stargazer -- report             # rank reflector modules, last 24 h
cargo run -p stargazer -- report --window-hours 6
```

The survey polls the [NJ6N DPLUSMON](https://nj6n.com/dplusmon/) feed
(a volunteer-run monitor that republishes every transmission on the
DPLUS network) rather than probing reflectors directly: no client
slots are consumed and no reflector sees a single extra packet. Please
be a good guest of that service too: the default 60-second interval is
gentler than one open browser tab of its own web UI (which polls every
15 s), a 30-second floor is enforced, errors back off exponentially,
and the tool identifies itself in its User-Agent.

The feed is a rolling ~30-row window, so history evaporates unless
kept. The survey archives everything it observes under `survey/dplusmon/`:

- `raw/<date>/<timestamp>.html`: every response, verbatim, before parsing
- `activity.jsonl`: append-only deduplicated transmission events
- `polls.jsonl`: per-poll provenance (row counts, window span, and a
  `gap_risk` flag when the window may have rolled over between polls)

`report` reads the accumulated archive and ranks reflector modules by
transmissions and distinct stations in the window. The longer the
survey runs, the more meaningful the ranking.

## Why archive raw AMBE?

Decoded audio is a one-way door: MP3/WAV destroys the codec
parameters. The raw 9-byte frames preserve everything the RF channel
delivered: they can be re-decoded forever with improving decoders,
and they are exactly the input that vocoder-parameter speech
recognition consumes (models that read AMBE/IMBE codec parameters
directly instead of reconstructed audio). The WAV exists for humans;
the `.ambe` exists for the future.

## Legal & community notes

- **License:** GPL-2.0-or-later.
- **Law and policy:** US federal law contains amateur-radio
  interception exceptions in
  [18 USC 2511(g)](https://uscode.house.gov/view.xhtml?req=%28title%3A18+section%3A2511%29)
  and
  [47 USC 605](https://uscode.house.gov/view.xhtml?edition=prelim&num=0&req=granuleid%3AUSC-prelim-title47-section605).
  Those provisions are not a blanket conclusion about recording,
  republication, transcription, privacy, or laws outside the US.
  Operators are responsible for applicable law and reflector/service
  policies.
- **Be a good guest:** stargazer occupies one client slot per
  configured module, exactly like any other linked station. Consider
  the reflector operator's policy and capacity before pointing a
  recorder at their system.
