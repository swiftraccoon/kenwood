# stargazer

Experimental D-STAR network observatory. The running service discovers
reflectors, monitors XLX activity, stores operational data in Postgres,
serves an HTTP API, and processes queued Rdio uploads. Voice-capture,
AMBE-to-MP3, and stream-finalization components are implemented and tested,
but the Tier 3 session orchestrator does not yet feed them live traffic.

## What this is

The D-STAR ecosystem has thousands of active reflectors across three
protocols (DPlus/REF, DExtra/XRF/XLX, DCS), but no unified monitoring
layer. XLX reflectors expose a UDP JSON push feed on port 10001;
REF/DCS reflectors expose nothing. Operator dashboards scrape HTML
per-reflector. There is no cross-network visibility layer.

`stargazer` is building that layer. It runs as a headless service, polls
multiple public data sources for reflector discovery, maintains live
XLX JSON monitor subscriptions to active reflectors, and exposes the stored
activity through Postgres and HTTP. Separate components decode AMBE to MP3
via the sibling [`mbelib-rs`](../mbelib-rs/) crate and upload completed
database rows to a Rdio API server (the
[`sdrtrunk-rdio-api`](https://github.com/swiftraccoon/sdrtrunk-rdio-api)
transcription pipeline) using the same wire format as SDRTrunk. Live
D-STAR sessions are not yet connected to that capture/upload path.

## Architecture: three-tier monitoring

### Tier 1 — Discovery & sweep (HTTP polling, no protocol connections)

- **Pi-Star hosts JSON** (`pistar.uk/downloads/DStar_Hosts.json`) —
  canonical list of ~4,200 reflector addresses, polled daily.
- **XLX API XML** (`xlxapi.rlx.lu/api.php?do=GetReflectorList`) —
  status of ~889 XLX reflectors with uptime/last-contact, polled
  every 10 minutes.
- **ircDDB last-heard HTML** (`status.ircddb.net/cgi-bin/ircddb-log`)
  — global D-STAR activity feed scraped every 60 seconds.

Output: reflector registry and activity log in Postgres.

### Tier 2 — XLX live monitoring (UDP JSON, port 10001)

xlxd reflectors accept a `"hello"` UDP datagram on port 10001 and push
real-time JSON events:

```
{"reflector":"XLX039  ","modules":["A","B",...]}
{"nodes":[{"callsign":"W1AW  B","module":"B","linkedto":"A",...}]}
{"stations":[{"callsign":"W1AW    ","node":"W1AW  B",...}]}
{"onair":"W1AW    "}
{"offair":"W1AW    "}
```

Stargazer maintains up to `max_concurrent_monitors` (default 100) of
these connections, selected from recent Tier 1 activity. No D-STAR
authentication or voice-protocol client slot is used; UDP port 10001 is
listed as xlxd's JSON interface in the upstream README. A monitor that
misses the implementation's 30-second receive deadline is dropped and may
be selected again on a later refresh.

### Tier 3 — Deep connect & voice capture (not yet orchestrated)

The planned Tier 3 path establishes full D-STAR protocol connections
(DPlus/DExtra/DCS) via the [`dstar-gateway`](../dstar-gateway/) crate's
`AsyncSession<P>` and feeds voice events through:

```
AsyncSession<P> → VoiceStart (header) → VoiceFrame × N → VoiceEnd
                    ↓
            mbelib-rs: AMBE [u8;9] × N → PCM [i16;160] × N
                    ↓
            mp3lame-encoder: PCM → MP3 @ 64 kbps
                    ↓
            Postgres streams table (metadata + audio_mp3 bytea)
                    ↓
            Rdio API upload: POST /api/call-upload (multipart)
```

Per-stream record: reflector, module, protocol, stream_id, callsign,
suffix, ur_call, D-STAR text (20-char message), DPRS lat/lon (if
present), started_at, ended_at, frame_count, encoded MP3 bytes.

The decoder, capture manager, stream finalizer, schema, and upload worker
exist. `tier3::run` currently logs its configuration and waits forever;
it does not create `AsyncSession` instances or persist captures.

## Rdio API upload (SDRTrunk-compatible)

Stargazer speaks the same `POST /api/call-upload` multipart wire
format as SDRTrunk, including the `User-Agent: sdrtrunk` header.
D-STAR concepts map to P25 concepts as:

| Rdio field | D-STAR value | Example |
|------------|--------------|---------|
| `system` | Protocol prefix + reflector number (REF=10k+N, XLX=20k+N, DCS=30k+N, XRF=40k+N) | `"10030"` (REF030) |
| `systemLabel` | Reflector + protocol | `"REF030 (DPlus)"` |
| `talkgroup` | Module as integer (A=1, B=2, ..., Z=26) | `"3"` (Module C) |
| `talkgroupLabel` | Module letter | `"Module C"` |
| `talkgroupGroup` | `"D-STAR"` | `"D-STAR"` |
| `source` | Raw callsign (string) | `"W1AW"` |
| `talkerAlias` | Callsign + suffix | `"W1AW / D75"` |
| `dateTime` | Stream start Unix timestamp | `"1712937600"` |
| `talkgroupTag` | D-STAR text message | `"Asheville NC"` |
| `audio` | MP3 bytes | *binary* |

## HTTP API

Lightweight operational endpoints — **not** the primary data consumer
(that's the Rdio API upload):

| Endpoint | Purpose |
|----------|---------|
| `GET /health` | Liveness/readiness probe |
| `GET /metrics` | Tier stats: reflectors, streams by status |
| `GET /api/reflectors` | List active reflectors |
| `GET /api/reflectors/{callsign}/activity` | Recent activity per reflector |
| `GET /api/reflectors/{callsign}/nodes` | Connected nodes per reflector |
| `GET /api/activity` | Global recent activity |
| `GET /api/streams` | Query captured streams (`?since=1h&reflector=REF030`) |
| `GET /api/upload-queue` | Pending uploads (failed streams are terminal — see `/api/streams`) |
| `POST /api/tier3/connect` | Manually promote reflector to Tier 3 (501 stub) |
| `DELETE /api/tier3/{callsign}/{module}` | Disconnect Tier 3 session (501 stub) |

## Configuration

TOML file (default `stargazer.toml`) with environment overrides:

```toml
[postgres]
url = "postgres://stargazer:pass@localhost/stargazer"
max_connections = 10

[rdio]
endpoint = "http://rdio-api:8080/api/call-upload"
api_key = "stargazer-key"
retry_interval_secs = 30
max_retries = 10

[tier1]
pistar = 86400        # Pi-Star poll interval (seconds)
xlx_api = 600         # XLX API poll interval (seconds)
ircddb = 60           # ircDDB scrape interval (seconds)

[tier2]
max_concurrent_monitors = 100
idle_disconnect_secs = 600  # reserved; not yet applied by the pool
activity_threshold_secs = 1800

[tier3]
max_concurrent_connections = 20  # reserved until Tier 3 is wired
idle_disconnect_secs = 300       # reserved until Tier 3 is wired
auto_promote = true              # reserved until Tier 3 is wired
dplus_callsign = "N0CALL"        # reserved until Tier 3 is wired

[audio]
format = "mp3"
mp3_bitrate = 64

[server]
listen = "0.0.0.0:8080"
```

Environment overrides: `STARGAZER_POSTGRES_URL`, `STARGAZER_RDIO_ENDPOINT`,
`STARGAZER_RDIO_API_KEY`, `STARGAZER_TIER3_DPLUS_CALLSIGN`,
`STARGAZER_SERVER_LISTEN`.

## Storage: Postgres as the durable spool

Postgres serves double duty as both the discovery registry and the
upload queue. Once Tier 3 writes completed streams, rows left with
`upload_status = 'pending'` during a Rdio API outage are drained when
the API recovers. Durable rows survive pod restarts without a separate
audio PersistentVolume.

Four tables: `reflectors`, `activity_log`, `connected_nodes`,
`streams`. Schema is embedded in the binary and applied idempotently
on startup.

## Legal & community notes

- **License:** GPL-2.0-or-later
- **Law and policy:** US federal law contains amateur-radio interception
  exceptions in [18 USC 2511(g)](https://uscode.house.gov/view.xhtml?req=%28title%3A18+section%3A2511%29)
  and [47 USC 605](https://uscode.house.gov/view.xhtml?edition=prelim&num=0&req=granuleid%3AUSC-prelim-title47-section605).
  Those provisions are not a blanket conclusion about recording,
  republication, transcription, privacy, or laws outside the US. Operators
  are responsible for applicable law and reflector/service policies.
- **Port 10001** is listed as the JSON interface in the
  [xlxd README](https://github.com/LX3JL/xlxd#firewall-settings). Confirm
  the reflector operator's policy and capacity before monitoring it.

## Status

Alpha. Tier 1 discovery, Tier 2 XLX monitoring, persistence, operational
HTTP reads, and the upload worker are implemented. Tier 3 is not end to
end: its capture manager, decoder, finalizer, and Rdio client are tested in
isolation, but session-pool orchestration and the manual Tier 3 API remain
stubs.
