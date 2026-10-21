# sextant

Desktop GUI client for D-STAR reflectors using DExtra, DPlus or DCS over UDP.
It decodes received AMBE voice and encodes microphone or WAV input. Connection,
meter and TX indicators describe the application's UDP and audio state; RF
transmission or reception requires independent observation.

## Operation

```text
cargo run -p sextant
```

The Operator page contains the received callsign, slow-data text, position,
elapsed time, received-frame counters and heard-station list. The Debug page
contains the event log, audio controls, WAV controls and a two-second silence
transmission. Settings contains operator identity, reconnect behavior, optional
heard-history persistence, audio-device selection and RX enhancement.

1. Set your operator callsign in Settings. Routing identities contain one through
   seven printable ASCII bytes. Form input is trimmed and ASCII letters are
   uppercased; saved reflector identities cannot contain `|`.
2. Open the reflector directory and choose a target, or enter its host, nonzero
   UDP port, protocol, callsign and modules manually. DNS names, IPv4 literals
   and IPv6 literals are accepted; IPv6 may be bracketed. DCS targets require
   an uppercase `DCS` prefix.
3. Click TRANSMIT or hold Space to send microphone audio. Release Space or click
   TX ACTIVE to stop. Space is suppressed while an editable field has focus;
   releasing it still stops a transmission started before focus changed.
4. Use Disconnect to cancel connection setup or retire an established session.

The default target is the bundled local POLARIS entry. To exercise a local
reflector, start `cargo run -p dstar-gateway-server --bin polaris` in another
terminal. Receiving your outgoing audio requires a second client on the same
reflector module.

Amber indicates received activity or navigation accents; red indicates active
outgoing audio; green indicates connection state or audio level. TX ACTIVE is
updated by the worker after its start request enters the session queue. Header,
voice and EOT send failures appear in the error strip and event log.

## Directory and connection setup

Directory sources are the bundled POLARIS target and the DPlus authentication
server's host list. DPlus-directory rows take precedence when a
callsign/protocol pair appears in both sources. Manual targets, favorites and
recents use checked addresses, ports, routing identities and modules. A recent
target is the submitted connection snapshot, even if its editable form changes
while connecting. At most 128 favorites and eight recents are retained.

A connection has one ten-second total budget for DNS resolution, optional DPlus
authentication and UDP handshake. DNS selects the first resolved address; the
local UDP socket uses that address's IP family. DPlus authentication failure
falls back to an empty host list before attempting the UDP link. A rejected link
is reported independently of that authentication result.

Only one directory fetch is retained at a time. Disconnect cancels it and any
pending connect or silence operation. Optional reconnect uses delays of zero,
three, ten and then twenty seconds; explicit Disconnect clears that schedule.

On the first load of settings from before the trusted-source address migration,
Sextant resets the current reflector target, favorites and recents. Unrelated
form settings remain. The persisted epoch marker preserves subsequent manually
entered targets; plaintext XLX registry caches are never loaded.

## Audio and files

Device input and output run on one dedicated thread. Named selections require
the named device; unavailable names are reported rather than substituted. The
supported device configuration is one through eight channels at 8 through
192 kHz using `f32`, signed 16-bit or unsigned 16-bit samples. RX enhancement
defaults to raw decoder output and can be toggled during a stream.

Local playback and file transmission accept 16-bit integer PCM WAV files,
one through eight channels, at 8 through 192 kHz, no larger than 64 MiB or five
minutes. Channels are averaged to mono. Files decode incrementally through a
bounded sinc-resampling buffer; malformed headers, unsupported formats and
sample errors are reported. File transmission emits at most one 160-sample
codec frame every 20 ms and resumes after a stall without a catch-up burst. Its
short final frame is zero-padded, then two zero-input frames release the
encoder's 40 ms lookahead before EOT.

The Debug page shows remaining file audio and separate Stop playback and Stop
transmission controls. Stopping local playback cancels decoding; samples already
in the one-second speaker ring drain as the device consumes them. Recording
captures received 8 kHz mono PCM in exclusively created WAV files under the
application configuration directory's `recordings` subdirectory. Stop recording
and application exit explicitly finalize and sync the recording; write,
finalization and sync failures remain visible.

Microphone and speaker rings each retain one second of samples at their device
rate. Overflow reports the rejected sample count. The ordinary audio queue
retains 256 operations and the session queue retains 32. A rejected operation is
reported; a retained EOT is retried before another outgoing stream can start.
Stop, playback-stop, recording-stop, Disconnect and shutdown have independent
control storage, so ordinary queue capacity cannot discard them.

### macOS microphone authorization

Create a bundle from the selected build profile:

```text
cargo run -p sextant -- --bundle
# For an optimized executable:
cargo run --release -p sextant -- --bundle
open target/Sextant.app
```

An optional path after `--bundle` selects another destination. The Rust command
copies the running executable and writes `Contents/Info.plist`, including
`NSMicrophoneUsageDescription`. Each file is synced and replaced individually;
success confirms both files. The command neither signs the enclosing bundle nor
grants microphone authorization.

Check System Settings > Privacy & Security > Microphone for the application's
authorization. Silent or low-level input also warrants checking the selected
device, mute and gain. See Apple's [media authorization documentation](https://developer.apple.com/documentation/bundleresources/requesting-authorization-for-media-capture-on-macos)
and [microphone privacy settings](https://support.apple.com/en-us/102071).

## Slow data and persistence

Outgoing text contains at most 20 printable ASCII bytes. Manual beacons use
checked coordinates, a primary-table APRS symbol and a printable ASCII comment
of at most 43 bytes. Both the D-STAR header and DPRS source use the identity
captured by the established connection.

Received GPS assembly retains at most 256 bytes including its terminator and
requires adjacent slow-data fragment pairs. Invalid lengths, byte values,
sequence gaps, oversized sentences and parse failures produce diagnostics.
DPRS verifies its CRC. The NMEA receive policy accepts checksumless sentences;
when a checksum is present it must match exactly. RMC requires active status;
GGA requires positive fix quality. Geographic fields require valid minute
values, matching hemispheres and checked latitude/longitude ranges.

Settings and heard history each have a 1 MiB encoded read/write limit. Settings
preserves partial editable strings with lossless quote, backslash and control
escaping. Saved targets are validated on restoration; invalid or duplicate rows
are skipped with a log diagnostic. History retains at most 100 stations, UTC
timestamps and checked restored positions. Its tab-separated message field
replaces tabs and line breaks with spaces.

Saving writes an exclusive temporary file, syncs it and renames it over the
snapshot. Unix success also syncs its containing directory; other platforms
report file sync plus rename. A directory-sync failure is reported after the
replacement has occurred. Errors appear in the GUI and logs.

GUI observations are bounded and current status is coalesced. Excess events or
audio diagnostics produce a loss count. On window exit, `main` requests session
shutdown, joins the audio worker after recording finalization, then observes the
session task's completion before destroying the runtime. Device-backend calls
and filesystem sync can delay that explicit cleanup.

## License

`GPL-2.0-or-later OR GPL-3.0-or-later`; see the workspace `LICENSE`. Embedded font
licenses are [Inter](assets/LICENSE-Inter.txt) and
[JetBrains Mono](assets/OFL-JetBrainsMono.txt).
