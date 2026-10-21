# stargazer

D-STAR recording, archive conversion, published-file collection and activity
surveying. The capture engine receives injected timestamps and performs no I/O.
The session shell owns UDP and the clock; explicit shutdown joins sessions and
pending filesystem work. Recording and recorder control require Unix sockets.
The format, harvest, survey, report and import APIs also compile on Windows.

## Configuration and capture

Configuration validates routing names, protocol identities, host addresses,
nonzero ports and required capture bounds. Reflector names are at most seven
ASCII bytes and safe as one filesystem component, including Windows reserved
names. This application's local module policy is A-E; target modules are A-Z.
Capture bounds are caller-selected storage policy, not protocol limits.

```rust
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use chrono::{DateTime, Utc};
use dstar_gateway_core::{StreamId, VoiceFrame};
use stargazer::audio::DecodedAudio;
use stargazer::capture::{CaptureManager, EndReason, StreamOrigin};
use stargazer::config::Config;
use stargazer::wav::WavFile;

let config = Config::from_toml(r#"
callsign = "N0CALL"
recordings_dir = "recordings"
max_capture_seconds = 3600
max_concurrent_captures = 4
[[record]]
reflector = "XRF001"
protocol = "dextra"
host = "127.0.0.1"
modules = ["B"]
"#)?;
let target = config.targets().first().ok_or("target")?.clone();
let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), target.port().get());
let mut captures = CaptureManager::new(
    StreamOrigin::new(target, peer),
    config.capture_limit(),
    config.concurrent_capture_limit(),
);
let stream = StreamId::new(0x1234).ok_or("stream")?;
let now = DateTime::<Utc>::UNIX_EPOCH;
assert!(captures.on_voice_frame(stream, 0, &VoiceFrame::silence(), now)?.is_none());
let recording = captures.on_voice_end(stream, EndReason::Eot, now).ok_or("recording")?;
assert_eq!(recording.expected_frames(), 1);
let audio = DecodedAudio::from_frames(recording.frames())?;
assert_eq!(audio.pcm().len(), 160);
let wav = WavFile::from_pcm(audio.pcm())?;
assert_eq!(wav.as_bytes().len(), 44 + 160 * 2);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Save equivalent TOML as `stargazer.toml`, then run:

```bash
cargo run -p stargazer -- --config stargazer.toml
cargo run -p stargazer -- ctl status
cargo run -p stargazer -- ctl disable XRF001-B
cargo run -p stargazer -- ctl enable XRF001-B
cargo run -p stargazer -- ctl reload
```

Protocol values are `dplus`, `dextra` and `dcs`; default ports are 20001, 30001
and 30051 respectively. `DPlus` authentication failure is recorded before the
configured unauthenticated connection attempt. A complete connect attempt has a
30-second deadline. Disabled targets persist in `.disabled-targets` under the
recording root. A file lock prevents concurrent recorder instances in that root.
Reload compares effective target identity, including protocol and endpoint.
Global configuration changes require a restart.

Reaching the per-stream duration writes the retained prefix with
`end_reason = "capture_limit"` and suppresses its remainder until the end event.
Exceeding the concurrent-state bound finalizes existing prefixes and reconnects.
Disable waits up to six seconds before reporting a still-retiring local session.
Replacements wait for the prior task to join; no response asserts remote slot
availability. Normal shutdown has no filesystem timeout and observes all tasks.
Process termination can interrupt that explicit shutdown.

```rust,no_run
use std::path::Path;
use std::sync::Arc;
use stargazer::config::Config;
use stargazer::control::Coordinator;
use stargazer::writer::Writer;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let path = Path::new("stargazer.toml");
let config = Config::load(path)?;
let writer = Arc::new(Writer::new(config.recordings_dir().to_owned(), config.write_wav()));
let mut recorder = Coordinator::new(config, path.to_owned(), writer)?;
recorder.spawn_initial();
let response = recorder.handle("status".parse()?).await;
println!("{}", response.message());
for completed in recorder.shutdown_all().await {
    let summary = completed.result?;
    println!("{}: {} recordings", completed.target, summary.recordings_written());
}
# Ok(())
# }
```

## Storage and formats

Artifacts live under `<root>/<REFLECTOR>-<MODULE>/<UTC date>/`. Stems contain the
UTC start time, sanitized station label and hexadecimal stream ID. Writers
exclusively reserve the whole stem and try up to 1000 suffix variants. Existing
artifacts are preserved. Each file is written to an exclusive temporary file,
synced and published by a non-replacing hard link. Raw AMBE is published first,
optional WAV second and JSON metadata last. A WAV failure is retained in
`audio.error`; raw-file failure prevents metadata publication.

On Unix the writer also syncs containing directories and newly created ancestor
entries. Other platforms omit directory syncing. Filesystem support and its sync
semantics determine persistence across power loss; no power-loss test is claimed.
Failures may leave partial sets or temporary files. `WriteError::MetadataVisible`
means complete JSON bytes are visible but a later completion step failed; inspect
that path before retrying. Imported recordings have `null` live endpoint/peer
metadata instead of fabricated network identities.

The version-1 `.ambe` header is 16 bytes: `STGZAMBE`, little-endian version 1,
little-endian record length 13 and four reserved bytes. Each record is one raw
sequence byte, nine AMBE bytes and three slow-data bytes in arrival order. All
256 sequence-byte values and reserved header bytes remain inspectable. Only
0 through 20 participate in inferred sequence gaps; duplicates infer no gap.

```rust
use stargazer::features::FrameArchive;

let mut bytes = Vec::from(*b"STGZAMBE\x01\x00\x0d\x00\x00\x00\x00\x00");
bytes.extend_from_slice(&[255; 13]);
let archive = FrameArchive::from_bytes(&bytes)?;
assert_eq!(archive.iter().len(), 1);
assert_eq!(archive.iter().next().ok_or("frame")?.seq, 255);
assert_eq!(archive.as_bytes(), bytes);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Decoded WAV is 8 kHz mono signed PCM16. One fresh decoder handles each stream;
inferred gaps call its concealment operation. RIFF counts and output allocations
are checked. `features::export_file` streams input records into 170 little-endian
f32 values per received or concealed frame and publishes only a complete output.
It preserves existing output files and rejects a truncated container.

```rust,no_run
use std::path::Path;
use stargazer::features::export_file;

let frames = export_file(Path::new("capture.ambe"), Path::new("capture.f32"))?;
println!("exported {frames} frames");
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ParsedDvrec::from_text` checks stream identity, header field widths and the
selected frame bound. `import_tree` also bounds input bytes and checks the
filename identity against recognized log lines. Per-file failures are counted
and logged; tree-walk errors fail the operation. Existing local counterparts are
preserved, including gapped captures.

```rust,no_run
use std::num::NonZeroUsize;
use std::path::Path;
use stargazer::capture::CaptureDurationLimit;
use stargazer::dvrec::{ImportLimits, import_tree};
use stargazer::writer::Writer;

let root = Path::new("recordings");
let limits = ImportLimits::new(
    NonZeroUsize::new(64 * 1024 * 1024).ok_or("byte bound")?,
    CaptureDurationLimit::try_from_seconds(3600)?,
);
let summary = import_tree(root, &Writer::new(root.to_owned(), true), limits)?;
println!("imported {}, failed {}", summary.imported, summary.failed);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Harvesting and surveying

Both HTTP clients identify the operator, enforce response-size bounds and follow
at most five redirects. Harvest policy follows
[RFC 9309](https://www.rfc-editor.org/rfc/rfc9309.html): matching agent groups,
longest wildcard/end-anchored match and allow ties; unreachable robots policy
prevents further collection. Downloads run sequentially, at least two seconds
apart, including after cancellation. Three consecutive failures stop the run.
Filename/path validation and non-replacing publication preserve existing files.
Matching station, stream ID and time selects candidates; it does not establish
byte identity or relative audio quality.

```rust,no_run
use chrono::NaiveDate;
use dstar_gateway_core::RoutingCallsign;
use stargazer::harvest::{HarvestOptions, Harvester};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let options = HarvestOptions::new(RoutingCallsign::new("N0CALL")?).with_dry_run(true);
let mut harvester = Harvester::new(options)?;
let target = "REF030-C".parse()?;
let date = NaiveDate::from_ymd_opt(2026, 7, 11).ok_or("date")?;
let report = harvester.harvest_dir(std::path::Path::new("recordings"), &target, date).await?;
println!("planned {}, failure {:?}", report.planned, report.failure());
# Ok(())
# }
```

Surveying collects rows returned by the configured NJ6N DPLUSMON endpoint.
Each poll start reserves a 30-second minimum interval before network I/O, retained
after cancellation. Complete response bytes are archived before parsing under
`<survey root>/dplusmon/raw/<date>/`; timestamp collisions receive unique suffixes.
Events append to `activity.jsonl`, and completed poll summaries to `polls.jsonl`.
An event enters in-memory deduplication only after its complete JSON line syncs.
Each failed append attempts truncate-and-sync rollback; a rollback failure retains
both errors. File locks serialize append transactions. Separate pollers have
separate deduplication sets, so use one surveyor for an archive.

```rust,no_run
use std::num::NonZeroUsize;
use std::path::Path;
use dstar_gateway_core::RoutingCallsign;
use stargazer::survey::{SurveyLimits, Surveyor};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let limits = SurveyLimits::new(NonZeroUsize::new(1_000_000).ok_or("identity bound")?);
let mut survey = Surveyor::new(Path::new("survey"), limits, RoutingCallsign::new("N0CALL")?)?;
let poll = survey.poll_once().await?;
println!("new {}, failure {:?}", poll.new_events, poll.failure());
# Ok(())
# }
```

Archive iteration streams a fixed-length snapshot, bounds each line and reports
malformed, oversized or unterminated JSON with path and line number. Survey
identity storage has a caller-selected bound; reaching it fails without silently
discarding history. Ranking incrementally reads events. The observed feed window
can omit transmissions; `gap_risk` records a window entirely newer than the
previous newest row. Archive operations are synchronous and may block.

```bash
cargo run -p stargazer -- survey --operator N0CALL --max-seen 1000000
cargo run -p stargazer -- report --window-hours 24
cargo run -p stargazer -- harvest --operator N0CALL --target REF030-C --dry-run
cargo run -p stargazer -- import-dvrec --max-capture-seconds 3600
```

The CLI defaults to 60-second polling, 1 MiB survey response/line and harvest
sidecar bounds, 64 MiB harvest response and import-input bounds, and a one-hour
imported capture bound. Import uses its input-byte bound for local sidecars too.
Survey/harvest use `--operator` or a valid recorder configuration. Published
content, retention and service policies depend on the endpoint. The license is
GPL-2.0-or-later; operators choose permitted sources and uses.
