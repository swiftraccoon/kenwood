# pulsar

Receive-only `BrandMeister` DMR call recorder over
[Open Terminal](https://github.com/swiftraccoon/kenwood/tree/main/dmr-rewind-core).
The library provides checked configuration, an I/O-free capture state machine,
JSON projection, blocking persistence and an owned asynchronous recorder. The
binary subscribes to configured group/private destinations and stores raw
AMBE+2 plus metadata. It sends no DMR voice and has no audio decoder or WAV output.
DMR AMBE+2 cannot be sent to the workspace's D-STAR AMBE 3600x2400 decoder.

## Configure and run

Copy the tracked `pulsar.example.toml` from the workspace root to `pulsar.toml`.
Use your seven-digit subscriber DMR ID and a master you can access. Consult
[`BrandMeister` account setup](https://help.brandmeister.network/dashboard/register-for-an-account/)
and [Hotspot Security](https://help.brandmeister.network/dashboard/hotspot-security/)
for service credentials. Passwords come only from named environment variables;
never store a password in TOML or logs.

```toml
recordings_dir = "dmr-recordings"
max_capture_records_per_call = 20000
call_hang_secs = 3
server_timeout_secs = 15

[[master]]
name = "chosen-master"
host = "3104.master.brandmeister.network"
port = 54006
dmr_id = 3101234
password_env = "PULSAR_CHOSEN_MASTER_PASSWORD"
bind = "0.0.0.0:0"
talkgroups = [91, 3100]
private_ids = []
```

The destination list is an example. Select traffic appropriate to your lawful
archival use and the practices of its community.

```sh
read -rs PULSAR_CHOSEN_MASTER_PASSWORD
export PULSAR_CHOSEN_MASTER_PASSWORD
cargo run -p pulsar -- check --config pulsar.toml
cargo run -p pulsar -- --config pulsar.toml
```

`check` loads configuration and referenced credentials without opening a
connection. Normal operation runs until Ctrl-C, finalizes open calls and drains
the writer queue. `cargo run -p pulsar -- --help` describes CLI arguments.

| Setting | Contract |
| --- | --- |
| `recordings_dir` | Nonempty path; defaults to `dmr-recordings`. |
| `max_capture_records_per_call` | Required nonzero count representable by the host. |
| `call_hang_secs` | Positive monotonic quiet interval; defaults to 3 seconds. |
| `server_timeout_secs` | Valid-packet timeout greater than 5 seconds; defaults to 15. |
| `[[master]].name` | Nonempty trimmed label, unique under ASCII case folding. |
| `[[master]].host` | IP literal or ASCII DNS name, without a scheme or port. |
| `[[master]].port` | Nonzero remote UDP port; defaults to 54006. |
| `[[master]].dmr_id` | Seven-digit outbound subscriber allocation. |
| `[[master]].password_env` | Name matching `[A-Za-z_][A-Za-z0-9_]*`. |
| `[[master]].bind` | Local endpoint, default `0.0.0.0:0`; port zero is ephemeral. |
| `[[master]].talkgroups` | Nonempty, distinct nonzero 24-bit group destinations. |
| `[[master]].private_ids` | Distinct nonzero 24-bit private destinations; defaults empty. |

DNS resolution is fallible network I/O and must return the bind's IP family.
All subscriptions for one master share one terminal stream. Adding destinations
does not provide simultaneous coverage of overlapping calls.

## Recording and completion

Files are grouped by sanitized master label, destination and UTC date. A
recording exists when its JSON sidecar exists. The writer atomically reserves
the whole stem across processes, writes/syncs temporary bytes, then installs
raw `.ambe` first and synced JSON last with non-overwriting hard links. The
filesystem must support same-directory hard links. On Unix, the directory
hierarchy and ancestor entries are synced before installation; the containing
directory is synced after each final file installation. Other platforms sync
file contents and make no directory-durability promise.

A crash may leave `.reserve`, `.tmp` or raw-only files; readers ignore those
without JSON, and later writes skip their stems. An error after JSON
installation can leave a visible complete pair, including a final directory-sync
error. Inspect JSON visibility before recovery; existing recordings are never
overwritten. Filesystem synchronization is blocking and has no overall timeout.

Completed calls pass through a bounded writer queue. A full queue pauses UDP
consumption and can lose datagrams. Persistence or capture-limit failure stops
the recorder. The retention limit is one budget shared by audio packets, voice
headers and embedded records. Overflow discards the entire partial call. A
20,000-record budget covers twenty minutes when every record is a 60 ms audio
packet; ancillary records consume the same budget.

UTC timestamps describe recordings. Independently supplied monotonic elapsed
readings determine inactivity, so UTC clock adjustments do not end calls early
or postpone expiry. Missing metadata stays explicit. A conflicting terminator
retains its raw bytes without replacing the active call's identity. Only an
observed terminator produces a terminator-ended call.

Version 1 stores the accepted 27 audio bytes unchanged, in arrival order. All
integers are little-endian:

```text
Header (16 bytes): magic [u8;8] = "PLSRAMBE", version u16 = 1,
                   record_len u16 = 34, reserved u32 = 0
Record (34 bytes): sequence u32, flags u16, subtype u8, ambe [u8;27]
```

The `pulsar-recording/1` JSON view contains connection/call identity, UTC timing,
end reason, counts, transport sequences, the raw filename, raw twelve-byte
voice headers, variable embedded bodies and an empty or twelve-byte terminator.
It retains master-provided callsign text as a lossy display view and exact ten
wire bytes separately as `callsign_wire_hex`. The raw-container version remains 1.

## Library workflows

```rust
use std::time::Duration;
use chrono::{DateTime, Utc};
use dmr_rewind_core::{DmrAudioSubtype, PacketFlags};
use pulsar::capture::{AudioPacket, CaptureManager, CaptureRecordLimit,
    CaptureTime, FinishReason, StreamOrigin};
use pulsar::config::Config;
use pulsar::writer::{container_bytes, RecordingDoc};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let config: Config = r#"
max_capture_records_per_call = 1
[[master]]
name = "local"
host = "127.0.0.1"
dmr_id = 3101234
password_env = "PULSAR_PASSWORD"
talkgroups = [91]
"#.parse()?;
let master = config.masters().first().ok_or("no master")?;
let origin = StreamOrigin::new(master, "127.0.0.1:54006".parse()?);
let now = CaptureTime::new(DateTime::<Utc>::UNIX_EPOCH, Duration::ZERO);
let packet = AudioPacket { transport_seq: u32::MAX.into(),
    flags: PacketFlags::from_bits(0x8001), subtype: DmrAudioSubtype::new(6)?,
    ambe: [0xA5;27] };
let mut capture = CaptureManager::new(origin.clone(), config.max_capture_records_per_call());
capture.on_audio(packet, now)?;
assert!(capture.on_audio(packet, now).is_err());
assert!(!capture.is_active());

let mut capture = CaptureManager::new(origin, CaptureRecordLimit::new(2)?);
capture.on_audio(packet, now)?;
let call = capture.finish(FinishReason::Shutdown,
    CaptureTime::new(now.wall(), Duration::from_millis(60))).ok_or("no call")?;
assert!(call.terminator().is_none());
let raw = container_bytes(call.audio_packets());
assert_eq!(raw.len(), 16 + 34);
assert_eq!(raw.get(23..50), Some(&[0xA5;27][..]));
let metadata = serde_json::to_value(RecordingDoc::from(&call))?;
assert_eq!(metadata.pointer("/audio/packets").and_then(serde_json::Value::as_u64), Some(1));
# Ok(())
# }
```

Completed records provide immutable accessors. Persistence adds the reserved raw
filename while retaining the stable JSON view:

```rust,no_run
use pulsar::{capture::CompletedRecording, writer::{Writer, WriteError}};
fn persist(call: &CompletedRecording) -> Result<std::path::PathBuf, WriteError> {
    Writer::new("dmr-recordings".into()).write(call)
}
```

`Recorder::wait` retains task ownership when its future is cancelled. Dropping
the handle requests shutdown; its coordinator continues draining on the
runtime. Keep the runtime alive and await completion to collect failures.
Signal-handler failure takes the same drain path as an ordinary interrupt.
Queue draining and blocking filesystem synchronization have no overall deadline.

```rust,no_run
use std::time::Duration;
use pulsar::{config::Config, session::{Error, Recorder}};
async fn record_for_one_minute(config: Config) -> Result<(), Error> {
    let mut recorder = Recorder::start(config)?;
    tokio::select! {
        completed = recorder.wait() => return completed,
        () = tokio::time::sleep(Duration::from_secs(60)) => {}
    }
    recorder.shutdown().await
}
```

This unpublished API is experimental. Configuration uses checked constructors,
accessors and `FromStr`; capture records carry canonical wire types; external
completion reasons cannot invent terminators; metadata projection is
`RecordingDoc::new`/`From`; and recorder handles expose explicit shutdown.

Pulsar is GPL-2.0-or-later.
