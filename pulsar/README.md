# pulsar

Receive-only BrandMeister DMR call recorder. Pulsar connects as a
software-only [BrandMeister Open Terminal][open-terminal-protocol], subscribes to
configured group and private IDs, and stores each received call as raw AMBE+2
plus metadata. It does not represent itself as an RF hotspot or repeater and
does not send DMR voice.

Pulsar deliberately does not decode audio. DMR uses AMBE+2, which the
repository's current software vocoder does not support, so recordings have no
WAV derivative. The raw container remains available for later playback or
decoding with a compatible vocoder.

## Before you start

Open Terminal setup is self-service; it does not require a Simple External
Application allocation from a master administrator:

1. Obtain your own seven-digit DMR ID from [RadioID.net][radioid].
2. [Register a BrandMeister account][bm-register] for that DMR ID.
3. In BrandMeister SelfCare, [set a Hotspot Security password][bm-hotspot].
4. Choose the BrandMeister master you will use for the terminal connection.

The terminal identity is your seven-digit DMR ID. Its authentication secret is
the Hotspot Security password you set in SelfCare, not your BrandMeister
account password, dashboard API key, or an administrator-issued application
password. Open Terminal normally uses port **54006/UDP**.

## Configuration

Copy the checked-in starter file, then replace the placeholder DMR ID and
review the selected master and destinations:

```bash
cp pulsar.example.toml pulsar.toml
```

The starter is equivalent to:

```toml
recordings_dir = "dmr-recordings"
max_capture_records_per_call = 20000
call_hang_secs = 3
server_timeout_secs = 15

[[master]]
name = "bm-us-chicago"
host = "3104.master.brandmeister.network"
port = 54006
dmr_id = 3101234
password_env = "PULSAR_CHOSEN_MASTER_PASSWORD"
bind = "0.0.0.0:0"
talkgroups = [91, 3100]
private_ids = []
```

Talkgroup 91 (Worldwide) and 3100 (USA) are examples only. Choose destinations
appropriate for the traffic you lawfully intend to archive and the communities
whose practices you will follow.

Never put the Hotspot Security password in TOML. `password_env` names the
environment variable from which Pulsar reads it. Supply the variable through a
service manager or secret store; for a local shell:

```bash
read -rs PULSAR_CHOSEN_MASTER_PASSWORD
export PULSAR_CHOSEN_MASTER_PASSWORD
```

The configuration fields are:

| Field | Default | Meaning |
|---|---|---|
| `recordings_dir` | `"dmr-recordings"` | Base output directory |
| `max_capture_records_per_call` | required | Nonzero ceiling shared by retained audio, voice-header, and embedded link-control records for one call |
| `call_hang_secs` | `3` | Quiet time after the last frame before an incomplete call is finalized |
| `server_timeout_secs` | `15` | No-receive interval before a master connection is considered lost; must exceed 5 seconds |
| `[[master]].name` | required | Unique operator label (sanitized for output paths) |
| `[[master]].host` | required | BrandMeister master hostname or IP |
| `[[master]].port` | `54006` | Open Terminal UDP port |
| `[[master]].dmr_id` | required | Your seven-digit subscriber DMR ID |
| `[[master]].password_env` | required | Environment variable containing your Hotspot Security password |
| `[[master]].bind` | `"0.0.0.0:0"` | Local UDP bind address |
| `[[master]].talkgroups` | required | Nonempty group-voice destination IDs |
| `[[master]].private_ids` | `[]` | Private-voice destination IDs |

Use a distinct environment-variable name if you configure more than one
credential.

## Check and run

From the repository root:

```bash
cargo run -p pulsar -- check --config pulsar.toml
cargo run -p pulsar -- --config pulsar.toml
```

`check` validates the file, subscription IDs, and referenced password
environment variables without starting the recorder. The normal command runs
until Ctrl-C, finalizes calls already in progress, and drains its recording
queue during orderly shutdown. `cargo run -p pulsar -- --help` is the
authoritative command summary if the CLI is being developed alongside this
README.

## Subscription behavior

All talkgroup and private-ID subscriptions in one `[[master]]` entry share one
Open Terminal stream. They are filters on that stream, not independent tuners
or simultaneous sessions. If subscribed calls overlap, only the call selected
for that terminal stream can be received and archived; adding IDs does not
provide concurrent coverage.

## Recording format

Each completed call has a raw `.ambe` container and a JSON metadata sidecar. A
recording exists **only when its JSON sidecar exists**. Pulsar writes and syncs
temporary data, installs the raw file first, and installs the synced JSON last
as the commit marker. On Unix it also syncs the containing directory after
each rename. A crash may leave a temporary file or an uncommitted raw file, but
readers must ignore either when the matching JSON is absent. Power-loss
durability on other platforms still depends on that operating system and
filesystem.

Completed calls enter a bounded writer queue, so normal file syncing does not
pause UDP reception. If storage remains slower than incoming completed calls
for long enough to fill the queue, the recorder applies backpressure rather
than growing memory without limit. Any persistence failure stops the process
with an error instead of silently discarding later calls.

Live call retention is also bounded. `max_capture_records_per_call` counts
each retained audio packet, voice-header record, and embedded link-control
record against one shared per-call budget. The example value permits 20 minutes
when every retained record is a 60 ms audio packet. A call that attempts one
more record is discarded in full and stops Pulsar with an explicit error;
Pulsar never writes a silently truncated recording. Choose a limit that covers
the longest call you intend to archive within the memory budget of the host.

The raw container preserves every accepted 27-byte Open Terminal DMR audio
payload in arrival order. All integers are little-endian:

```text
Header (16 bytes):
  magic       [u8; 8] = "PLSRAMBE"
  version     u16     = 1
  record_len  u16     = 34
  reserved    u32     = 0

Records (34 bytes each):
  sequence    u32       Open Terminal transport sequence
  flags       u16       Open Terminal transport flags
  subtype     u8        DMR audio subtype
  ambe        [u8; 27]  encoded AMBE+2 payload, unchanged
```

The JSON is the searchable call record. It identifies the configured master
and terminal DMR ID, group/private session, source and target identities when
the superheader supplied them, timing and end reason, audio counts, transport
sequences, and raw filename. It also retains raw 12-byte voice-header Full Link
Control, 10-byte embedded Link Control, and the final terminator. Some masters
send the terminator with 12-byte Full Link Control and some send it empty;
Pulsar preserves either form. A timeout-ended or otherwise incomplete call has
no terminator record. Missing metadata remains explicit rather than being
inferred from a later call.

There is deliberately no WAV field or file. Do not feed these frames to the
D-STAR AMBE 3600x2400 decoder: DMR AMBE+2 is a different codec.

## Security, law, and community

Open Terminal is an unencrypted UDP service. Keep the terminal host and secret
restricted, use a trusted network path, and do not expose the local listener
unnecessarily.

Use Pulsar only for private, lawful, receive-only archival. Access to traffic
does not by itself grant permission to transcribe or republish it. Follow
applicable law, BrandMeister policy, and the practices of each talkgroup
community. This is not legal advice.

Pulsar is GPL-2.0-or-later.

[open-terminal-protocol]: ../dmr-rewind-core/README.md
[bm-register]: https://help.brandmeister.network/dashboard/register-for-an-account/
[bm-hotspot]: https://help.brandmeister.network/dashboard/hotspot-security/
[radioid]: https://radioid.net/account/register
