# TM-D750 REPL

`tmd750-repl` is a plain-text USB and native macOS Bluetooth shell
for the Kenwood TM-D750. It is designed
to work well with a screen reader. It supports normal CAT control,
bounded CAT/MMDVM diagnostics and guarded MCP configuration workflows.

The REPL can select FM or
D-STAR DV on either band, and every selection requires an immediate readback
from the radio. It reads persistent DV Gateway state, naming the observed
values Off and Terminal. Ordinary CAT does not change that setting.
`dstar start` returns `ConfigurationError::UnsupportedProfile` before endpoint
discovery, capture creation or radio writes: a Kenwood Terminal `SET_CONFIG`
layout has not been verified. Generic public MMDVM layouts are not assumed to
configure Kenwood hardware. The diagnostic's explicit `--manage-terminal`
option remains a separate diagnostic-only entry/restoration workflow.
It does not expose arbitrary CAT commands or arbitrary MCP memory writes.
The schema-driven `mcp menu` commands discover, inspect, and preview registered
settings. Ordinary scalar updates are guarded on firmware 1.02 by complete
captured-page comparison, immediate full-page readback, and fresh CAT
verification, and are tested against mock transports rather than per field on
hardware. Gateway, transport, and automatic-transmission fields require the
dedicated lifecycles below and are rejected by the ordinary menu setter.
The PM1, MY1 and channel-name setters are separate workflows. Offline inspection decodes any
TM-D750 backup with the registry layout; every live write requires
`TM-D750 / 1.02 / K,2,1`.

Captured workflows create a new private session directory. An explicit
`--output` requires an existing parent. The default `captures/` base is created
on Unix, and its containing directory is synchronized before a session is
reserved or an endpoint is opened. On other platforms, default ancestor
synchronization returns `Unsupported`; provide `--output` with a prepared
parent. Successful filesystem synchronization is the host's persistence
boundary; it is not a hardware power-loss guarantee.

## Run

Connect USB or pair the radio with your Mac, leave Gateway Off, and run:

```bash
cargo run -p tmd750-repl
```

Ordinary CAT commands prefer an available TM-D750 USB endpoint. When no USB
endpoint is present, macOS automatically selects the single recognized paired
Bluetooth radio. No address is required. Selection verifies the radio's CAT
identity before control; a connection failure does not switch to another radio.

USB discovery recognizes main-unit and control-panel endpoints. Multiple USB
endpoints require an explicit choice:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101
```

To use Bluetooth while USB is connected:

```bash
cargo run -p tmd750-repl -- --bluetooth
```

The default baud rate is the hardware-validated 9600 baud. Use `--baud` only
when testing a deliberately different configuration.

The TM-D750 library owns USB discovery and the RTS/CTS, DTR, and RTS preset;
`kenwood-transport` supplies shared physical serial I/O and transport contracts.
Endpoint selection and post-MCP readiness remain explicit REPL policies, not
automatic behavior in the shared transport. MMDVM diagnostic parsing comes
from `mmdvm::probe`, with no TH-D75 library dependency. The REPL owns the
TM-D750 identity check, the selected connection, and final transport close.

Read-only CAT status is exercised on hardware over both main-unit and
control-panel USB with firmware 1.02. A USB identity only selects a candidate
endpoint; the CAT identity exchange is what proves the radio.
The transport does not automatically reopen after a USB disconnect: serial
pathnames can be reassigned. Reconnect the intended radio and select its current
endpoint for a new session.

### Native Bluetooth control and backups on macOS

The shell, CAT diagnostics and MCP reads share paired-radio
selection. Force Bluetooth with a flag; no MAC address is needed:

```bash
cargo run -p tmd750-repl -- --bluetooth
cargo run -p tmd750-repl -- --bluetooth status
cargo run -p tmd750-repl -- --bluetooth mode b
cargo run -p tmd750-repl -- --bluetooth \
  mcp probe --output bluetooth-probe-01
cargo run -p tmd750-repl -- --bluetooth \
  mcp backup --output bluetooth-backup-01
```

Automatic selection recognizes paired names `TM-D750` and the observed
`stm32mp1-ex5240`. Names only select a candidate; CAT proves its identity.
Multiple candidates are listed without opening any of them. Use
`--bluetooth-address 01:23:45:67:89:AB` to select an exact address instead,
replacing the example with your radio's address. This override implies
Bluetooth and bypasses inventory; a separate `--bluetooth` flag is unnecessary.
The selected address is retained internally for the whole workflow.

Without a startup command, the native shell uses the same CAT vocabulary as
USB. It identifies the radio and requires Gateway Off before accepting input.
Every setting write and transmit command except `rx` additionally requires a
fresh Gateway Off reply, plus the library's firmware/type gate; setting writes
are verified by echo and readback. Mode selections do not enable Terminal Mode.
Invalid input stays local. A failed CAT operation closes the captured
connection without retrying a command or reopening it.

Rich terminals retain line editing and session history. Plain terminals,
including `TERM=dumb`, and pipes use cancellable input. EOF, `quit`, and Ctrl-C
retire the radio and input independently. A started mode write finishes its
echo/readback before cancellation is handled. Redirected regular-file scripts
are loaded and size-checked before opening the radio, with a 1 MiB limit; commands
are limited to 4 KiB before the newline. Pipes and files are batch sessions:
invalid commands stop the script with a nonzero exit status, and cancellation
does not count as successful completion. Interactive typos leave the prompt
available. Startup CAT commands do not read stdin.
Enter one command per rich-terminal prompt; use a pipe or file for multi-command
scripts.

Read-only startup `identity` (or `id`), `status`, and `gateway` retain their
diagnostic report path. Shell sessions, fixed MCP probes, and standard backups
also capture observations and cleanup in private directories. Omitting
`--output` for a probe or backup creates a directory under `captures`; CAT
uses that default destination. Programming temporarily interrupts normal radio
operation, even without settings writes. `mcp text set` and `mcp menu apply`
also run over native Bluetooth, as described under
[Settings writes over native Bluetooth](#settings-writes-over-native-bluetooth);
the reentry probe remains USB-only. `dstar start` currently refuses its
unverified modem configuration before I/O. Offline MCP inspection remains
available.

Native MCP probe and backup require `TM-D750 / 1.02 / K,2,1` and a fresh Gateway
Off reply. The probe reads two fixed fragments totaling 295 bytes. Backup uses
the library's complete standard schedule: 1,138 pages and 289,962 bytes, with
no settings writes. Both require the exit acknowledgment. The original
connection is retained without further
protocol traffic for five seconds, then closed and dropped. Recovery is
attempted only after complete read/exit results, a clean original close, and
complete captures. Recovery reopens the exact address and the RFCOMM channel
from that original successful connection, then verifies the original CAT
identity and Gateway Off. The five-second wait is host scheduling policy, not a
firmware readiness signal. The first command on a native connection waits up
to ten seconds for its reply, and every later exchange 1.5 seconds, because
the radio holds the first reply of a connection opened within about five
seconds of the previous close until about five seconds after that close. After
a programming exit the radio answers no earlier than about 20 seconds after
the exit acknowledgment, so the fresh connection's first command waits until
30 seconds after the end of the read that exited, and never less than ten
seconds, however quickly the connection reopened.

Each opening phase permits at most two attempts, separated by one second, for
eligible native opening failures. A retry uses a new helper after the previous
helper has been reaped. A typed opening failure can leave the native channel
close unconfirmed; a retry does not cancel an operation the operating system
still owns. Cancellation, helper launch/framing errors, an invalid or changed
endpoint, incomplete capture, and a failed cleanup on a late successful
connection prevent another attempt. Neither MCP nor a CAT exchange is retried. In
particular, a changed identity or Gateway reply stops recovery.

Format-3 native diagnostic reports record the first-reply deadline and the
post-exit reply bound, retain every opening attempt, including a failed
attempt before eventual success, and distinguish initial SDP discovery from
fixed-channel recovery; format 2 lacks both deadlines. Earlier format-1 captures retain their original single-attempt
policy and are read under it. This is ordinary
MCP-to-CAT recovery, not the separate Terminal-to-MMDVM transition lifecycle.

Native standard backups use a distinct format-4 report tagged
`transport = "native_bluetooth"`; pages are in `workflow.original.backup.segments`.
Successful reports require complete original and fresh opening histories,
acknowledged pages and exit, the captured five-second retention wait, clean
closes, and matching fresh identity/Gateway Off. Offline show, preview,
preflight, and comparison accept these captures and format-3 native backups,
which lack both deadlines, each under its own recorded policy. They
record the native transport and cannot
serve as the USB backup required by existing live settings writers or managed
diagnostics. Interactive, batch, and one-shot mode sessions instead publish
`operation = "cat_session"`, format 1, with their actual input mode and
independent input, output, protocol, capture, and cleanup errors; these are not
configuration backups.

Opening failures identify the observed host stage, such as an SDP completion
deadline or an RFCOMM opening deadline. The command prints each failed attempt
and retains it in the report, together with any independent cleanup failure.
A stage identifies where progress stopped, not the underlying cause in the
operating system or radio.

The initial opening performs a fresh Serial Port Profile service query and
records the selected address and RFCOMM channel from that same open. Recovery
pins that channel for this workflow, using the fixed-channel baseband wakeup
before RFCOMM opening. The channel is not a permanent model constant. It does not
guess a channel from another radio model or fall back to the operating system's
serial alias. Paired-name discovery happens before opening and pins one exact
address; the CAT identity exchange still proves the radio, and recovery reopens
only that address. Neither `--bluetooth` nor `--bluetooth-address` can be combined
with `--port` or `--baud`. Packaged installations can supply a trusted executable
with `--bluetooth-helper /absolute/path/to/helper` alongside either Bluetooth
option; the same helper performs inventory and opening. Ordinary builds include
the helper. Help and offline inspection do not launch it or discover devices.

Native SPP CAT and fixed MCP read/exit support the
`TM-D750 / 1.02 / K,2,1` tuple. Every native setting write requires a fresh
Gateway Off reply. The first reply uses a ten-second host deadline; later
exchanges use the regular deadline. Consecutive one-shot commands require no
added delay.

A Bluetooth backup whose pages and exit ACK all succeed still fails if the fresh
`ID` after reopening receives no bytes within its post-exit first-reply
deadline; offline inspection rejects such a report despite its complete page
data. A macOS
connected flag or a cached SDP service record does not imply a usable SPP
session. Keep other applications off the selected radio connection while
running diagnostics.

Bluetooth MCP-to-CAT recovery requires a fresh matching CAT identity and
Gateway Off on the pinned channel, clean closes, and synchronized transcripts.

### Settings writes over native Bluetooth

`mcp text set` and `mcp menu apply` run over native Bluetooth with the same
journal, captures, compare-write-verify sessions and reports as over USB.
Select the radio with `--bluetooth` or `--bluetooth-address` instead of
`--port`, and give a native Bluetooth backup of that radio as the source:

```bash
cargo run -p tmd750-repl -- --bluetooth mcp backup --output bluetooth-backup-01
cargo run -p tmd750-repl -- --bluetooth \
  mcp text set --backup bluetooth-backup-01/report.json --expect PM1 --apply \
  --output bluetooth-pm1-01 pm-name-1 BASE
```

The backup's address names the radio the write reaches, so a USB backup is
refused. The command validates its options, the backup and the requested value
before it selects a radio, and refuses before opening anything when the
selected radio is not the backup's. Each programming session opens the exact
address, with a Serial Port service query the first time and the pinned
RFCOMM channel afterwards, and requires Gateway Off before programming entry.
After the acknowledged exit it holds the connection without traffic for five
seconds, closes it, reopens the pinned channel, and reads the identity tuple
and Gateway Off, the first reply allowed until 30 seconds after the exit.
Reports carry `transport = "native_bluetooth"` and the address and channel as
their endpoint; text updates use format 16 for every setting and menu updates
format 2.

Text-update reports use versions 13, 14 and 15 for USB PM1, MY1 and channel
names, and version 16 for native Bluetooth. Their scope labels describe the
actual identity and Gateway checks. Text journals use version 2. Configuration
backup loaders retain their documented earlier backup formats and reject
text-update reports as backups.

## Configuration backup and PC text entry

Read the standard configuration through a dedicated USB connection:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp backup --output backup-01
```

The directory must be new and its parent must exist. Omitting `--output`
reserves a unique private directory under `captures`. The command reads the
official standard configuration schedule: 1,138 pages containing 289,962 bytes,
including all six Programmable-Memory slots. It excludes the custom startup
bitmap and unclassified regions. It sends no settings writes or RF requests.
Programming temporarily interrupts normal radio operation. `--position-memory`
also reads the GPS position memory's fourteen pages after the standard ones in
the same session, into `backup.position_memory` with the report `scope`
`standard_configuration_and_position_memory_without_startup_screen`; it runs
over USB only.

The USB command closes and drops the original handle after the exit ACK, waits two
seconds, then uses [bounded CAT reacquisition](#bounded-cat-reacquisition).
One sixty-second dispatch window covers passive enumeration, up to six fresh
connections, and two-second retry waits. Every open requires the original path
and unchanged recognized VID/PID. Only a completely silent initial `ID` reply
timeout permits another connection; partial replies, identity mismatches, and
open, write, close, or capture failures stop recovery. This retries only
post-exit identity checks, never MCP entry, configuration reads, or exit. An
explicit `--port` is mandatory. Cancellation finishes the current exchange;
incomplete framing prohibits speculative exit or recovery commands.

The USB `report.json` uses format version 4 with `operation = "configuration_backup"`.
Its `backup.segments` retain every fully acknowledged page even if a later page,
cleanup, or fresh verification fails. Addresses and lengths are explicit; unread
gaps are absent. Original and fresh transcripts remain separate.
`post_exit_verification` records policy limits, elapsed readiness time, every
entry in `attempts`, the aggregate transcript result, and the final outcome.
Earlier silent timeouts remain recorded when a later identity check succeeds.
A successful exit requires the entire page schedule, acknowledged MCP exit,
clean closes and complete captures for every connection, matching fresh identity,
and a written, flushed, synchronized report.

Offline inspection and update planning accept complete successful USB format-4
backups and historical USB format-3 backups under their original single-attempt
policy. The loader checks the report shape declared by its format version; it
never upgrades a report or accepts a failed capture, and it describes the stored
pages rather than the radio's current state. Reports must be regular files no
larger than 32 MiB
and contain every standard page in order with exact addresses and lengths.
Native Bluetooth backups use their separate schema described above. They are
accepted for offline inspection and previews, not by USB write workflows.

This is a standard-region backup, not a full memory dump or a restorable `.d750`
file. The official application seeds omitted bytes from its current model;
knowing the file header does not justify filling those gaps with guessed values.
A compatible export still needs a validated matching template. Keep captures
private: they can contain callsigns, messages, and other personal settings.

### General keyboard menu control

Start here for menu discovery and ordinary scalar settings. These commands use
the shared menu registry and update planner, not a separate setter for each
field. Discovery and backup inspection do not enumerate or open radio ports:

```bash
cargo run -p tmd750-repl -- mcp menu list --group dv
cargo run -p tmd750-repl -- mcp menu describe pm.PmName2
cargo run -p tmd750-repl -- mcp menu show \
  --backup backup-01/report.json pm.PmName2
cargo run -p tmd750-repl -- mcp menu preview \
  --backup backup-01/report.json --output preview-01.json pm.PmName2 "Base Camp"
```

Keys come from `list`. `describe` lists storage bounds, public choice labels,
raw choice values, and write policy. The registry currently has 431 fields:
350 ordinary, 60 lifecycle-required, 15 with unresolved value domains, and six
binary fields, as classified by the generated registry. Binary fields have no
scalar preview or setter. Unknown stored numeric values remain visible when read.
`describe` emits JSON without prose wrapping or timestamp prefixes so its output
can be consumed directly by other tools.

Global fields reject `--slot`. Per-slot fields require an explicit `--slot`
from 0 through 5: 0 is PM Off, and 1 through 5 are the corresponding PM profiles.
For example, inspect or preview the stored MY1 text in PM Off:

```bash
cargo run -p tmd750-repl -- mcp menu show \
  --backup backup-01/report.json --slot 0 \
  'dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway'
cargo run -p tmd750-repl -- mcp menu preview \
  --backup backup-01/report.json --slot 0 \
  'dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway' KQ4NIT
```

Quote field keys containing brackets and values containing spaces. Input is not
lowercased, trimmed, truncated, or converted from guessed display units. Numeric
values are stored integers; unsigned decimal and `0x` hexadecimal are accepted.
Booleans accept `true/false`, `on/off`, `yes/no`, and `1/0`. Public enum labels
are case-insensitive when unambiguous. Fixed strings use their declared encoding
and exact padding. An empty string is an explicit clear when its field permits
it. Embedded terminators and padding collisions are rejected.

Additional ordinary-value constraints apply beyond storage width. Group links
accept 0 through 29 or 255 for not linked; the MY selector accepts 0 through 5;
the DV-message selector accepts 0 for Off or 1 through 5; coordinate-direction
fields accept 0 or 1. The six MY callsign fields additionally enforce supported
call-text shape and a conservative uppercase ASCII letters/digits/spaces policy.
This validates the requested representation, not callsign ownership or Terminal
acceptance. Setting MY1 does not itself enter Terminal Mode.

To leave an ordinary value configured, select either TM-D750 USB endpoint,
main unit or operation panel, provide a successful current backup from that
radio, and pass `--apply`:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp menu apply --backup backup-01/report.json --apply \
  --output menu-update-01 pm.PmName2 "Base Camp"
```

Further fields join the same session through repeatable `--and FIELD VALUE`
pairs; `--slot` binds every per-slot field of the run and must be omitted when
every field is global, and a field may appear once:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp menu apply --backup backup-01/report.json --slot 0 --apply \
  --output menu-update-02 pm.PmName2 "Base Camp" --and radio.Beep off
```

A live run requires `TM-D750 / 1.02 / K,2,1`, a main-unit or operation-panel
USB endpoint at 9600 baud, memory format zero, a valid active PM selection,
and Gateway Off. Complete format,
PM-control, active-Gateway, and target pages must all match fresh reads before
the first write. A mismatch aborts; the command does not silently merge newer
radio settings into the old backup. Equal target pages are compared without
writes. Unrelated bytes and neighboring bits remain exact.

Each changed page requires a synchronized raw transcript and a private journal
containing both complete images before dispatch. There is one W/ACK and one
immediate complete-page readback per changed page. After acknowledged MCP exit,
the original connection closes and drops; a fresh connection must confirm the
same CAT identity and Gateway Off, then close cleanly, through the
[bounded CAT reacquisition](#bounded-cat-reacquisition) that backups use on
either USB role.
This is **one MCP session followed by CAT reads only**, so it does not check
that the change survives MCP re-entry or a power cycle. A reused USB pathname
does not identify the physical unit.

Keep the same radio connected and close other radio applications. Ctrl-C can
cancel before the first write intent. Afterward, the planned batch and its
verification finish unless a protocol, capture, journal, or cleanup failure
prevents them. Multi-page updates are not firmware-atomic. Incomplete exchanges
prohibit speculative exit, retry, reconnect, or automatic rollback. Earlier
verified writes remain reported if a later page fails. The command sends no RF
request; separately configured radio functions can still transmit.
Programming interrupts normal operation.

The new private directory contains a format-3 `report.json` with
`operation = "menu_update"`, `menu-journal.jsonl`, `transcript.jsonl`, and
`post-exit-transcript.jsonl`. Retain them on failure; do not restore stale pages
blindly. Journal preparation retains every complete before/after image, including
unchanged guards. Report `compared_pages` describes a successful whole batch;
partial comparisons remain in the raw transcript. `possible_pages` includes
verified writes as well as uncertain dispatches. Success additionally requires
the final report to be written, flushed, and synchronized. Live updates require
Unix private-file and directory synchronization support. An update report is
not a new configuration backup;
refresh the backup before the next edit.

The ordinary policy accepts registry fields and their checked value domains;
it accepts neither arbitrary addresses nor firmware overrides.

### Dedicated PM1 setter with re-entry verification

The `pm-name-1` form of `mcp text set` changes the global PM1 label. Start with a successful
configuration backup from the connected radio, then supply the expected current
name, the replacement name, and `--apply`:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp text set --backup backup-01/report.json --expect PM1 --apply \
  --output pm1-update-01 pm-name-1 "Home"
```

This command leaves `Home` in place; it does not restore `PM1`. Names must be
1 through 16 printable ASCII bytes. Case and spaces are preserved; input is
never truncated. Either TM-D750 USB role, main unit or operation panel, at
9600 baud and the TM-D750 / firmware 1.02 / type K,2,1 identity are
accepted. The separate MY1 form is described below;
all other text settings, per-slot selectors, arbitrary addresses, and firmware
overrides are refused. Equal expected and
replacement names are rejected before radio access, not reported as a live
confirmation of the current setting.

Before writing, the entire freshly read PM1 page must match the backup, not
just its name. The command synchronizes the raw pre-write transcript and a
private journal containing both complete pages, sends one page write, and
compares its immediate readback.
A second, read-only MCP session checks that the complete desired page survives
exit/re-entry. Each session requires exit ACK, a clean original close/drop,
and a separate fresh CAT identity check with a clean close, through the
bounded silent-`ID` retry that backups use, because the operation-panel
endpoint answers `ID` only once its tuple is ready. The second session, and on
the operation-panel endpoint the first
session as well, opens only after the selected endpoint has been enumerated
continuously for ten seconds within a sixty-second budget: an absence restarts
the period, because the operation-panel endpoint can re-enumerate once more
after it first answers `ID`, and an endpoint that never settles is recorded as
that session's open failure. This is four connections in total when every
check matches on its first open, plus one per silent `ID` attempt.
It requests no other setting change or RF transmission.

Keep the same radio connected and close other radio applications; a pathname and
the public identity tuple do not identify the physical unit. Ctrl-C can cancel
before write intent; afterward the command finishes the remaining verification
steps. A failure stops the workflow. Uncertain framing permits no speculative
exit, retry, or automatic rollback. Retain the journal if a write may have
occurred; do not blindly restore an old page. If programming exit is unconfirmed,
stop using the uncertain connection and retain the report and journal.
The program sends no recovery bytes or automatic retry. This workflow requires
Unix private-file and directory synchronization support.

The output directory must be new. It contains a format-13 `report.json` with
`operation = "pm1_name_update"`, `update-journal.jsonl`, and separate transcripts
for both MCP sessions and both fresh CAT checks. The final status distinguishes
`not_written`, `possibly_changed`, and `verified_across_sessions`. A report
that ends at `possibly_changed` is resolved with a fresh `mcp backup` compared
page by page against the pre-write backup through `mcp terminal compare`, not
by rerunning the command. These files contain private settings. An update
report is not a configuration backup: **take a new full backup before the
next edit**, since the old PM1 page is stale.

The supported live-write tuple is `TM-D750 / 1.02 / K,2,1` over USB.
`verified_across_sessions` requires the immediate page readback, acknowledged
MCP exit, a second MCP read of the desired page, and fresh CAT identity checks.

### Dedicated PM Off MY1 setter with re-entry verification

The `dv-gateway-my-callsign-1` form of `mcp text set` leaves a requested DV
Gateway MY1 value configured in PM Off. For a currently empty MY1, use an
explicitly empty expected value:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp text set --backup backup-01/report.json --expect "" --apply \
  --output my1-update-01 dv-gateway-my-callsign-1 KQ4NIT
```

To replace existing text, supply that exact text to `--expect`. Callsigns must
contain one to eight uppercase ASCII letters, digits, or spaces, including at
least one letter or digit. Spaces remain exact. The command neither normalizes
input nor inserts a suffix; its eight-byte storage uses NUL padding. An empty
desired value, slash, lowercase input, non-ASCII text, or no-op is refused
before radio access. `--clear` writes eight NUL bytes, the stored form of an
empty MY1, and is a no-op against an empty expected value. This validates
storage text, not ownership or Terminal acceptance.

Both the backup and fresh reads must show PM Off, Gateway Off, and MY1 selected.
Either TM-D750 USB role at 9600 baud and the exact `TM-D750 / 1.02 / K,2,1`
identity are required. Before writing, the entire fresh target and control pages must match
the immutable backup. Verification compares the entire desired target page and
the unchanged control page. Only MY1's eight bytes can change; its memo, other MY entries, Gateway
mode, selection, and every unrelated byte are preserved. No other PM slot,
routing change, Gateway activation, RF command, or automatic rollback is allowed.

The two-session lifecycle shares PM1's capture and journal protections. MY1
additionally requires fresh CAT Gateway Off before both MCP entries and after
both exits; the post-exit check is the bounded silent-`ID` retry followed by
one `GW` query on the matched attempt. Original
and fresh connections must close cleanly, and the journal
and transcripts must be synchronized before the next session. Cancellation
before intent prevents new connections; after possible dispatch, verification
finishes, while any failed requirement stops without retry or stale restoration.

The new private directory contains a format-14 report with
`operation = "my1_callsign_update"`, the immutable `update-journal.jsonl`, and
separate original/fresh transcripts for both sessions. Keep these files private.
Take a new full configuration backup before another edit; an update report is
not a backup. The same Unix private-file and directory-sync requirements apply.

The supported live-write tuple is `TM-D750 / 1.02 / K,2,1` over USB.
The CAT `callsign` command reads a different list from this Gateway MY1 field;
verify this field through its captured memory page.

### Dedicated channel name setter with re-entry verification

The `channel-name` form of `mcp text set` writes one memory channel's
sixteen-byte name and leaves it in place. Select the channel with `--channel`
using the CAT selector (`000` to `999`, `L00` to `U49`, `Pri`), give the
expected current name (`--expect ""` for an unnamed channel), and either the
replacement name or `--clear`:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp text set --backup backup-01/report.json --expect "" --apply \
  --channel 999 --output channel-name-01 channel-name "Repeater 1"
```

Names must be 1 through 16 printable ASCII bytes; case and spaces are
preserved and nothing is truncated. The stored field is NUL padded, and
`--clear` writes sixteen NUL bytes, which is how an unnamed channel is stored.
Either TM-D750 USB role, main unit or operation panel, at 9600 baud is
accepted, with the exact TM-D750 / firmware 1.02 / type K,2,1 identity. Names
are not part of the CAT channel record, so this is the only way this program
writes one.

The whole 256-byte name page holding the channel must match the backup before
the single write, so the other fifteen names on that page, including the
weather-channel names that share the last page, are carried unchanged and
compared in full. The lifecycle, journal, captures and report are those of the
PM1 form: a format-15 `report.json` with `operation = "channel_name_update"`
and a `channel` field, `update-journal.jsonl`, and transcripts for both
sessions and both fresh CAT checks. Take a new full backup before the next
edit.

### Several channel names in one session

`mcp channel-names apply` writes the names of any number of channels in one
programming session. Each `--set CHANNEL NAME` names a channel and each
`--clear CHANNEL` leaves one unnamed; a channel may appear once.
`mcp channel-names preview` takes the same arguments offline and lists the
name-table pages that change:

```bash
cargo run -p tmd750-repl -- mcp channel-names preview \
  --backup backup-01/report.json --set 995 "Batch 995" --set 996 "Batch 996"
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp channel-names apply --backup backup-01/report.json --apply \
  --output channel-names-01 --set 995 "Batch 995" --set 996 "Batch 996"
```

The pages are planned against the backup: every changed 256-byte name page
must match it before the first write, is read back after its write, and the
other names on it are carried unchanged. A plan that changes no page opens
nothing. The run uses the menu-update lifecycle, journal and fresh CAT check:
a format-1 `report.json` with `operation = "channel_names_update"` and
`channel-names-journal.jsonl`. Store the channel records over CAT first with
`store`: a CAT store keeps a channel's name and a CAT `clear` erases it, so
names written before a clear do not survive it.

### Memory group names

`mcp group-names list` prints a backup's thirty memory group names and the
name of the weather channels' group. `mcp group-names preview` and `mcp
group-names apply` rename any number of groups in one session: each `--set
GROUP NAME` gives group 0 to 29 a name of 1 to 16 printable ASCII bytes, and a
group may appear once. The radio's defaults are `GRP-0` to `GRP-29`:

```bash
cargo run -p tmd750-repl -- mcp group-names list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp group-names apply --backup backup-01/report.json --apply \
  --output group-names-01 --set 3 Repeaters --set 20 "Simplex 2m"
```

Groups 0 to 15 share the name page from `0x14800` and the rest the page from
`0x14900`; each changed page must match the backup before its write and is
read back after it. The run uses the shared page-update lifecycle, journal and
fresh CAT check: a format-1 `report.json` with `operation =
"group_names_update"` and `group-names-journal.jsonl`, whose prepared entry
records each group's name before and after. `apply` runs over either TM-D750
USB role at 9600 baud and requires `--apply` and an explicit `--port`; native
Bluetooth refuses it.

### Channel groups

`mcp channels list` prints each stored regular channel's memory group. `mcp
channel-groups preview` and `mcp channel-groups apply` move any number of
stored regular channels (000 to 999) to other groups in one session: each
`--set CHANNEL GROUP` names a channel once and a group from 0 to 29. A CAT
`store` resets a channel's group to its hundreds digit, so assign groups after
storing:

```bash
cargo run -p tmd750-repl -- mcp channel-groups preview \
  --backup backup-01/report.json --set 995 12
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp channel-groups apply --backup backup-01/report.json --apply \
  --output channel-groups-01 --set 995 12 --set 996 12
```

The group is byte 2 of the channel's flag record, in the flag-table pages from
`0x2000`; each changed page must match the backup before its write and is read
back after it. The run uses the shared page-update lifecycle, journal and
fresh CAT check: a format-1 `report.json` with `operation =
"channel_groups_update"` and `channel-groups-journal.jsonl`, whose prepared
entry records each channel's group before and after. `apply` runs over either
TM-D750 USB role at 9600 baud and requires `--apply` and an explicit `--port`;
native Bluetooth refuses it.

### D-STAR repeater and hotspot lists

`mcp repeater list` prints a backup's repeater list (1,500 slots) or, with
`--hotspots`, its hotspot list (30 slots) in the radio's chain order, with each
record's world region, country and group names. `mcp repeater preview` and
`mcp repeater set` edit one stored record in place: give its `--slot` from
the list and any of `--frequency`, `--offset` (MHz), `--shift minus|plus`,
`--name`, `--sub-name`, `--callsign` and `--gateway`:

```bash
cargo run -p tmd750-repl -- mcp repeater list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp repeater set --backup backup-01/report.json --apply \
  --output repeater-01 --slot 364 --frequency 448.380
```

An edit keeps the record's slot and its links, so the list order is
unchanged; the whole page holding the record must match the backup before the
write and is read back after it. The run uses the menu-update lifecycle,
journal and fresh CAT check: a format-1 `report.json` with
`operation = "repeater_update"` and `repeater-journal.jsonl`, whose prepared
entry records both versions of the record. `tuning dr` then shows the edited
repeater when it is the selected one.

`mcp repeater names` prints every named or coded country and group with its
index, name and voice code, the up to four uppercase letters and digits the
official program keeps for it. `mcp repeater voice preview` and `mcp repeater
voice set` change one code: give `--country` (0 to 149) or `--group` (0 to
299) and `--code` or `--clear`. The code's page must match the backup before
the write and is read back after it; the run uses the same lifecycle, journal
and report as a record edit, whose prepared entry records the code before and
after:

```bash
cargo run -p tmd750-repl -- mcp repeater names --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp repeater voice set --backup backup-01/report.json --apply \
  --output voice-01 --country 1 --code IND
```

`mcp repeater set` and `mcp repeater voice set` run over either TM-D750 USB
role at 9600 baud and require `--apply` and an explicit `--port`; native
Bluetooth refuses them.

### D-STAR callsign list

`mcp callsign-list list` prints a backup's destination callsign list (300
slots) in display order, numbering the entries from 1. `mcp callsign-list
preview` and `mcp callsign-list apply` edit it: `--remove POSITION` drops the
entry `list` numbered, and `--add NAME CALLSIGN MEMO` appends one (`""` for no
memo); both repeat, and removals count positions in the backup's list:

```bash
cargo run -p tmd750-repl -- mcp callsign-list list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp callsign-list apply --backup backup-01/report.json --apply \
  --output callsign-01 --add "REF001 C" REF001CL "Link reflector 1 C"
```

The edited list is stored compact from the first slot. Every changed page,
including the page that holds the list's ends beside the repeater list's,
must match the backup before its write and is read back after it. The run
uses the shared page-update lifecycle, journal and fresh CAT check: a
format-1 `report.json` with `operation = "callsign_list_update"` and
`callsign-list-journal.jsonl`, whose prepared entry records the list before
and after. `apply` runs over either TM-D750 USB role at 9600 baud and
requires `--apply` and an explicit `--port`; native Bluetooth refuses it.

### Stored, CALL and weather channels

`mcp channels list` prints every stored memory channel of a backup, then the
six CALL channels and the ten weather channels. `mcp call-channel preview` and
`mcp call-channel set` give one CALL channel (`--channel vhf-fm`, `vhf-dv`,
`220-fm`, `220-dv`, `uhf-fm` or `uhf-dv`) a new `--frequency` in MHz and keep
the rest of its record. The frequency must lie in the channel's band: 136 to
174 MHz, 216 to 260 MHz or 410 to 470 MHz, the upper bound excluded:

```bash
cargo run -p tmd750-repl -- mcp channels list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp call-channel set --backup backup-01/report.json --apply \
  --output call-01 --channel uhf-fm --frequency 446.100
```

The page holding the record must match the backup before the write and is
read back after it. The run uses the shared page-update lifecycle, journal and
fresh CAT check: a format-1 `report.json` with
`operation = "call_channel_update"` and `call-channel-journal.jsonl`, whose
prepared entry records the frequency before and after. On a band tuned to the
channel's range in the channel's mode, `tuning a call` then `channel a` reads
the record back. `set` runs over either TM-D750 USB role at 9600 baud and
requires `--apply` and an explicit `--port`; native Bluetooth refuses it.

### Memory channel stores and clears

`mcp channels preview` and `mcp channels apply` store and clear any number of
regular, program scan and priority channels (`000` to `999`, `L00` to `L49`,
`U00` to `U49`, `Pri`) in one session. `--store CHANNEL SOURCE` takes band A's
or B's current VFO record in PM Off (`a` or `b`, the record `FO` reports) or
another stored channel's record and lockout; `--clear CHANNEL` empties a
channel and its name. A store writes the band code, lockout and record a CAT
`store` of the same record writes and keeps the channel's group and name; a
clear writes what a CAT `clear` leaves. CAT refuses the priority channel; this
command writes it:

```bash
cargo run -p tmd750-repl -- mcp channels preview \
  --backup backup-01/report.json --store Pri a
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp channels apply --backup backup-01/report.json --apply \
  --output channels-01 --store Pri a --store 041 040 --clear 040
```

Each changed flag, record and name page must match the backup before its write
and is read back after it. The run uses the shared page-update lifecycle,
journal and fresh CAT check: a format-1 `report.json` with `operation =
"memory_channels_update"` and `memory-channels-journal.jsonl`, whose prepared
entry records each channel before and after and the CAT fields of each stored
record. `apply` runs over either TM-D750 USB role at 9600 baud and requires
`--apply` and an explicit `--port`; native Bluetooth refuses it.

### Mic-E type codes

`mcp mic-e list` prints a backup's three user Mic-E type code tables
(`kenwood-handheld`, `kenwood-mobile` and `yaesu`, five entries each), which
add radio models for the station list to name Mic-E stations. `mcp mic-e
preview` and `mcp mic-e apply` edit one entry: give its `--family` and
`--position` (1 to 5) and either `--code` (one printable ASCII character other
than space) with `--model` (at most 15 printable ASCII characters), or
`--clear`:

```bash
cargo run -p tmd750-repl -- mcp mic-e list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp mic-e apply --backup backup-01/report.json --apply \
  --output mic-e-01 --family yaesu --position 1 --code _ --model FT-99
```

The tables' page must match the backup before the write and is read back
after it. The run uses the shared page-update lifecycle, journal and fresh CAT
check: a format-1 `report.json` with `operation = "mic_e_update"` and
`mic-e-journal.jsonl`, whose prepared entry records the entry before and
after. `apply` runs over either TM-D750 USB role at 9600 baud and requires
`--apply` and an explicit `--port`; native Bluetooth refuses it.

### VFO memory

`mcp vfo list` prints a PM slot's VFO memory from a backup (`--slot`, 0 to 5,
default 0): each band's current VFO record and selected VFO band, then the
record and programmable limits of every VFO band, four on band A and six on
band B, the slot's Auto Repeater Offset setting and the receive step band B
keeps for each step range. `mcp vfo preview` and `mcp vfo set` make one edit.
`--tune BAND MHZ` sets band `a` or `b` to a new receive frequency as the
radio's frequency entry does: it selects the VFO band whose limits hold the
frequency, starts from that VFO band's stored record when it is not the
selected one, takes a new band B step segment's receive step, rounds the
frequency down to the receive step, and under the slot's Auto Repeater Offset
takes the TM-D750A plan's shift; it stores the record as both the band's
current record and that VFO band's record, and band B's receive step as its
step segment's.
`--limits VFO_BAND LOWER UPPER` sets one VFO band's programmable limits in
whole megahertz:

```bash
cargo run -p tmd750-repl -- mcp vfo list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp vfo set --backup backup-01/report.json --apply \
  --output vfo-01 --tune b 223.040
```

Each changed page must match the backup before its write and is read back
after it. The run uses the shared page-update lifecycle, journal and fresh
CAT check: a format-1 `report.json` with `operation = "vfo_update"` and
`vfo-journal.jsonl`, whose prepared entry records the change before and
after. `set` runs over either TM-D750 USB role at 9600 baud and requires
`--apply` and an explicit `--port`; native Bluetooth refuses it.

### DTMF and EchoLink memories

`mcp dtmf list` prints a backup's two automatic-dialer memories, each with
channels `CH0` to `CH9`. `mcp dtmf preview` and `mcp dtmf apply` replace or
clear one channel: give its `--memory` (`dtmf` or `echolink`) and `--channel`
(0 to 9), then `--name`, `--code` or both, or `--clear`. A DTMF name or code
holds at most 16 characters and an EchoLink one at most 8; a name is printable
ASCII and a code holds `0` to `9`, `A` to `D`, `*`, `#` and a space for a
pause. An omitted `--name` or `--code` stores none:

```bash
cargo run -p tmd750-repl -- mcp dtmf list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp dtmf apply --backup backup-01/report.json --apply \
  --output dtmf-01 --memory echolink --channel 1 --name KX0NODE --code 123456
```

Each changed page must match the backup before its write and is read back
after it: a DTMF channel changes the code page from `0x38` and the name page
from `0x100`, and an EchoLink channel the page from `0x200`. The run uses the
shared page-update lifecycle, journal and fresh CAT check: a format-1
`report.json` with `operation = "dtmf_memory_update"` and `dtmf-journal.jsonl`,
whose prepared entry records the channel before and after. `apply` runs over
either TM-D750 USB role at 9600 baud and requires `--apply` and an explicit
`--port`; native Bluetooth refuses it.

### GPS position memory

`mcp position list` prints the positions of a backup taken with `mcp backup
--position-memory`, numbered from 1 in the radio's order. `mcp position
preview` and `mcp position apply` edit them: `--remove NUMBER` drops a
position and `--add NAME LATITUDE LONGITUDE` appends one, the name at most
nine stored bytes and the coordinates in signed decimal degrees (negative
south and west) rounded to a ten-thousandth of a minute. Added positions take
the current UTC time, `--altitude` (default 0, stored as given) and
`--symbol` (an APRS table then code, default `\K`, the official program's
default):

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp backup --position-memory --output backup-01
cargo run -p tmd750-repl -- mcp position list --backup backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp position apply --backup backup-01/report.json --apply \
  --output position-01 --add HOME 35.6995 139.7707 --symbol /-
```

The positions are stored from slot 0 in order and listed in the index page,
so an edit writes the record pages from `0x4C000` and the index page
`0x4CF00` that change. Each must match the backup before its write and is read
back after it. The run uses the shared page-update lifecycle, journal and
fresh CAT check: a format-1 `report.json` with `operation =
"position_memory_update"` and `position-memory-journal.jsonl`, whose prepared
entry records the positions before and after. `apply` runs over either
TM-D750 USB role at 9600 baud and requires `--apply` and an explicit `--port`;
native Bluetooth refuses it.

The physical unit of stored altitude is unverified. The input integer is
stored unchanged.

### Offline text inspection

`mcp text list`, `show`, and `preview` work entirely offline, without enumerating
or opening USB:

```bash
cargo run -p tmd750-repl -- mcp text list
cargo run -p tmd750-repl -- mcp text show \
  --backup backup-01/report.json pm-name-1
cargo run -p tmd750-repl -- mcp text preview \
  --backup backup-01/report.json \
  --output pm-name-preview.json pm-name-1 "Local repeaters"
```

Supported fields include five PM names, the six DV Gateway MY callsigns and
their memos, five D-STAR messages, and the power-on message. Per-slot fields
require an explicit `--slot 0` through `--slot 5`; global PM names reject
`--slot`. `mcp text list` reports each stable key, scope, encoding, and
encoded-byte limit. Input is never silently truncated or case-folded, and
control characters are rejected. Field encoding is not a claim of radio-side
callsign syntax validation.

Every TM-D750 backup is decoded with the registry layout. The reader requires
a complete successful configuration capture and verifies every requested field
byte was captured before decoding it.

Preview prints the existing and proposed text without changing the source.
An optional new private format-2 JSON file records the source capture and its
identity, before/after text, and exact masked page changes. It records
`radio_applied: false`. Existing files are never overwritten. Preview writes
nothing to the radio; the dedicated PM1, MY1 and channel-name setters provide
live text writes, and ordinary scalar settings use the separate menu apply
workflow.

## Offline Terminal settings preflight

Inspect the Terminal-related settings in a successful configuration backup:

```bash
cargo run -p tmd750-repl -- mcp terminal preflight \
  --backup backup-01/report.json --slot 0 --interface main-usb
```

`--slot` explicitly selects PM 0 (PM Off) or PM 1 through 5;
`--interface` selects `main-usb` or `panel-usb`. These are requested settings to
compare, not instructions to switch PM or connect a USB port. The report
distinguishes the requested slot from the active PM recorded in the backup,
and the requested interface from the captured DV Gateway route. It also
reports the selected MY callsign and whether it is empty, USB function,
Terminal subtype, DV Gateway mode, and the stored RPT1/RPT2 text.

All observations describe the stored capture, not the radio's current state.
The command neither normalizes
values nor guesses missing callsigns or identification suffixes; a suffix finding
asks for operator review rather than testing radio-side acceptance. Captured
settings that agree with the request say nothing about MMDVM availability,
Internet access, or a working reflector connection.

This command does not enumerate or open radio interfaces, activate Terminal
Mode, generate write patches, or change the backup or radio. Use it to review a
captured configuration before setup.

## Compare configuration captures

Compare two successful configuration-backup reports: USB format 4, historical
USB format 3, or native Bluetooth format 3:

```bash
cargo run -p tmd750-repl -- mcp terminal compare \
  --before backup-01/report.json --after backup-02/report.json \
  --slot 0 --interface main-usb
```

Every standard configuration byte is compared: 1,138 pages and 289,962 captured
bytes across all six PM slots. Unread gaps and the startup bitmap are excluded.
`--slot` selects only the two offline preflights, not a comparison filter;
`--interface` supplies their intended USB route without selecting an endpoint.

The finite software-layout catalog labels candidate changes in 116 fields
covering 560 bytes: the global format and active-PM selectors, plus menus 980,
986, 650, 651, 670, 671, and 672 in every PM slot, including all six MY callsigns
and their memos. Changes outside that catalog remain in the comparison and are
counted explicitly; the catalog is finite, so such a change can still affect
Terminal operation. Unknown or invalid values encountered by either
selected-slot preflight are recorded as failures without discarding the raw
differences. Memos, unselected MY entries, and other slots receive raw-byte
attribution, not semantic validation.
An otherwise successful comparison exits successfully even if either preflight
fails; the console and private report explicitly retain that failure.

Console output shows counts, page addresses, and candidate field keys, but no
captured text or byte values, including when a preflight fails. Add
`--output terminal-comparison-01.json` to save exact differences, source paths
and identities, interpreted selected-slot
text, and detailed preflight errors. The file must be new and its parent
directory must already exist. Unix creation requests mode `0600`, subject to
the process umask; other platforms use their default or inherited permissions.
Choose a private directory and keep the report private. Its format is 2 and
its operation `terminal_configuration_comparison`; it is not a backup or a
write plan.

Both inputs must pass coverage and completion checks and have identical model,
firmware, and full radio-type identities. These are structural checks on the
report contents, not authentication of the capture, and matching identities do
not identify one physical radio. Before/after order is taken from the arguments
and is not verified. No USB enumeration, radio or RF access, source-file
changes, setting writes, or write-gate changes occur.

When validating settings on hardware, compare one intentional
preparation-field change at a time with DV Gateway Off initially. PM recall is
a separate state-changing operation, not a read-only check. Enabling Menu 650
may remove CAT/MCP access on the selected interface, so do not assume that
activation can be followed by another backup on that port. Protocol handoff,
return to CAT, and restoration are not validated on hardware.

## Restore a configuration backup

`mcp restore preview` and `mcp restore apply` restore one standard backup,
`--from`, onto the radio whose current standard backup is `--backup`; the two
must have the same model, firmware and type. `--from-file` restores a `.d750`
configuration file instead: its header must name the radio's type, and only
its bytes in the standard configuration schedule are read. Every page whose
bytes differ is written with the restored bytes, except that the lifecycle
settings keep their current values: the active PM, the PC and data
interfaces, Bluetooth, the DV Gateway and Terminal selection, and the
automatic transmissions. The preview lists the changed pages and each
lifecycle setting the restored configuration holds differently:

```bash
cargo run -p tmd750-repl -- mcp restore preview \
  --backup backup-02/report.json --from backup-01/report.json
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp restore apply --backup backup-02/report.json --from backup-01/report.json \
  --apply --output restore-01
```

Each changed page must match `--backup` before its write and is read back
after it, so take `--backup` just before the restore. The run uses the shared
page-update lifecycle, journal and fresh CAT check: a format-1 `report.json`
with `operation = "configuration_restore"` and `restore-journal.jsonl`, whose
prepared entry records the source (`backup` or `d750`) and its path, the pages
and bytes written and the settings kept. `apply` runs over either TM-D750 USB role at 9600 baud
and requires `--apply` and an explicit `--port`; native Bluetooth refuses it.

The live-write tuple is `TM-D750 / 1.02 / K,2,1` over USB. Every changed
page requires its complete current image to match the backup before dispatch
and its complete readback to match the replacement afterward.

## Read-only MCP re-entry control

`mcp reentry-probe` is a read-only MCP re-entry diagnostic, not a Terminal Mode
setter. It requires macOS, an explicit main-unit USB endpoint at 9600 baud,
and `--approve-live-test` for two programming-mode interruptions:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp reentry-probe --approve-live-test --output reentry-01
```

Each session requires exact identity `TM-D750 / 1.02 / K,2,1` and fresh `GW 0`
before entry. It reads only `8..48` and `327681..327936`, acknowledges exit,
closes and drops the old handle, then runs the
[bounded CAT reacquisition](#bounded-cat-reacquisition) with one `GW` query on
the matched connection for the fresh identity/Gateway-Off check. Session 1's captures
must be complete and synchronized before session 2 opens. There are no settings
writes, fill commands, RF commands, retries, or arbitrary memory addresses.

Private captures include separate original/fresh transcripts for both sessions,
an append-only session journal, and timestamped raw macOS serial-registry
observations. An initial successful, synchronized observation precedes opening.
The observer waits 50 ms after each sample; actual sample durations and gaps
must be read from its timestamps. Registry IDs identify host-side registrations,
not physical radios or full-radio reboots. Sampling cannot exclude shorter
detachments. Subprocess or capture failure is an error, never endpoint absence.
Recording and synchronization add host latency; neither is a readiness remedy.

Ctrl-C finishes the current exchange and exits an entered, synchronized session,
then closes without starting another connection. That cancelled probe is
incomplete. Uncertain entry, read, ACK, or exit stops active commands; only
close/drop follows, with up to 20 seconds of passive observation when not
cancelled. No speculative exit or recovery request is sent.

Success requires both read sessions, both fresh CAT-Off checks, every close,
complete synchronized captures, and a joined successful observer. A successful
pair is not a readiness bound. Reading 295 distinct bytes twice is not a
configuration backup and does not show that other settings are unchanged.

## Fixed MCP probe

The startup-only `mcp probe` command captures a small, fixed protocol check. It
reads CAT identity, enters MCP programming mode, reads 40 bytes at address 8
and 255 bytes at address 327681, then requires MCP exit, releases the original
handle, and uses bounded, identity-only CAT reacquisition. It sends
no settings writes, fill commands, arbitrary memory requests,
or RF transmission requests. The 295 bytes it reads are a protocol check, not a
backup. Programming mode temporarily interrupts normal radio operation even
though no settings are written.

USB disconnects and re-enumerates after MCP exit, so the command never reuses
the original handle; it requires a fresh identity exchange on a new connection,
because a returned USB device path is only an endpoint.

Leave the radio on its normal screen and close other radio applications first:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp probe --output bench-01
```

The explicit output directory must not exist; its parent must already exist.
Omit `--output` to create a unique session directory below `./captures/`.
Existing captures are never overwritten. All three output files are reserved before
the serial connection opens. On Unix the directory is created with permissions
0700 and the files with 0600. Windows uses inherited filesystem permissions.
On Windows, use an explicit output directory with an existing parent; default
capture-base synchronization is unsupported.

An explicit `--port` before `mcp` is required. Only enumerated TM-D750 USB
identities are accepted; no unrelated serial ports are opened or probed and
no endpoint is automatically substituted. Recognition of the panel's USB
identity selects the endpoint; the MCP exchange itself decides whether the
protocol works. The command cannot run
inside the CAT prompt; quit that prompt and start a dedicated process.

Each capture contains:

- `report.json`: format version 3, software version, UTC start and finish,
  selected USB path and VID/PID, CAT baud, original-session identity,
  accepted entry reply, acknowledged fragments, exit disposition, outcome,
  separate fresh-connection verification, and connection, signal, capture,
  or close failures.
- `transcript.jsonl`: sequential transport events with exact UTC Unix
  nanoseconds, monotonic elapsed microseconds, raw byte arrays, baud changes,
  and transport errors. A write request is recorded before dispatch; a separate
  completion or failure records the transport result. A failed write does not
  prove how many bytes reached the radio. Read events contain only returned
  bytes, including empty arrays for zero-length reads. Each event is flushed
  independently of optional trace logging.
- `post-exit-transcript.jsonl`: passive endpoint observations and every fresh
  open, identity attempt, and close, with one aggregate capture summary.

Report fragments have explicit addresses and lengths; gaps are not fabricated
into a full memory image. Uniform-fill responses are expanded in the report,
while the transcript retains the actual response bytes. Error objects preserve
the message and underlying cause chain. A successful protocol check is
`probe.outcome.status = "awaiting_cat_verification"`; exit and transcript
completeness are reported separately. Overall success additionally requires
`post_exit_verification.outcome.status = "matched"` and clean closes/captures.
The process exits nonzero on cancellation, a final failed readiness outcome,
or any original protocol, capture, signal, or connection-close failure. An
eligible silent identity timeout can precede a successful fresh attempt; the
report retains that earlier failure rather than rewriting it as successful.

Ctrl-C requests cancellation after the current complete exchange. If the
connection remains synchronized, MCP exit and the original close still finish.
Cancellation before fresh verification skips the new connection; cancellation
during an identity attempt lets that bounded attempt and close finish without
retry. A transcript write failure requests the same boundary-safe stop without
interrupting cleanup. After an incomplete protocol
exchange, the tool sends no speculative recovery bytes or automatic retry.
Stop using an uncertain connection and retain the transcript. Do not kill the
process to shorten a pending exchange or close.

An abrupt process termination can leave `report.json` empty, and a filesystem
failure can leave a partial final transcript line. Captures may contain private
radio settings; review them before sharing. Nothing uploads or deletes them
automatically.

### Bounded CAT reacquisition

Both `mcp probe` and `mcp backup` stop original-handle protocol traffic at the
MCP exit ACK; neither CAT nor a baud change follows that ACK. Each workflow
closes and drops that handle before considering a fresh connection. An
incomplete read, missing exit ACK, original close or capture failure, or
cancellation prevents the additional open. The explicit `--port` is required
before `mcp`; the tool never substitutes an automatically selected port.

On the operation-panel endpoint, `mcp probe`, `mcp backup` and `mcp menu
apply` open the endpoint for programming entry only after it has been enumerated continuously
for ten seconds within a sixty-second budget, because that endpoint
re-enumerates once more about ten seconds after a programming exit, after it
first answers `ID`: a backup started 0.3 seconds after a matched readiness
check found the endpoint gone (`ENXIO`) when it sent the entry command. The
main-unit endpoint, which re-enumerates before it answers, opens at once.

After the two-second settle wait, one sixty-second readiness dispatch window
covers passive USB enumeration, identity attempts, closes, and waits between
retries. Enumeration polls at 250 ms intervals while the selected endpoint is
absent. Before every fresh open, the exact original path must be enumerated
with unchanged recognized VID/PID. Conflicting metadata, multiple same-role
endpoints, or a different path stop verification; even a macOS callout/dial-in
alias is not substituted for the selected path. No unrelated port receives
CAT traffic.

At most six fresh connections can be opened. Each sends only `ID`, `FV`, and
`TY`, compares the complete identity tuple with the original, and closes. Each
query has separate 1,500 ms write and reply deadlines; close has a two-second
bound. Eleven seconds for a complete identity attempt and close must remain
both before opening and after the open returns. Insufficient remaining time
stops new identity traffic without abandoning cleanup. Synchronous OS open or
enumeration calls and host scheduling can exceed these limits, so the policy
does not promise a hard wall-clock completion deadline.

A retry is admitted only when the first `ID` query times out after its write
completed, no input bytes arrived, the handle closed and dropped successfully,
and the complete transcript was synchronized. The tool waits two seconds,
checks cancellation and the shared remaining budget, then re-enumerates before
another open. Any partial reply, later `FV` or `TY` failure, write timeout,
other transport read error, open failure, identity mismatch, close failure, or
capture failure stops without retry. Ctrl-C finishes the current bounded
identity attempt and close, then prevents another connection.

Reacquisition sends no MCP entry or exit, reset, setting, Gateway, packet-exit,
or baud-change commands. These limits are conservative host policy, not
measured firmware timing requirements or a guarantee of recovery. On panel USB,
silent `ID` replies before a successful one are expected on this firmware.
Text and menu updates use the same reacquisition on either USB role; the
reentry probe admits main-unit USB only. One `GW` query follows the matching
identity where Gateway Off is required. The managed diagnostic reuses bounded
identity-only reacquisition after its guarded exchanges. Ordinary CAT and
gateway sessions do not acquire automatic reconnect behavior.

Original-session fields remain separate: a successful read-and-exit phase has
`probe.outcome.status` or `backup.outcome.status` equal to
`"awaiting_cat_verification"`. The required `post_exit_verification` object
records the policy limits, elapsed readiness time, an `attempts` array, one
aggregate `transcript` summary, and the final `outcome`. Each array entry retains
its `enumerations`, optional `connection` with open/identity/close results, `outcome`, and
`retry_admission`. A later match does not erase an earlier timeout; exhausting
the budget or attempt cap is a final readiness failure. The single
`post-exit-transcript.jsonl` is reserved before the initial radio open and
records timestamped waits,
enumerations, open requests/results, and actual fresh-connection transport
events. Snapshots include recognized radio endpoints and metadata needed to
explain conflicts, not unrelated serial or Bluetooth inventory.

Enumeration times in the report share the readiness window's start, after the
settle wait; they do not restart for each attempt. When both absence and
presence are observed, the timestamped transcripts bracket those observations
relative to the exit ACK and record when the fresh CAT responses complete.
These are host observations with polling and scheduling uncertainty, not exact
firmware-ready timestamps.

Exit status is zero only when the original fragments, exit ACK, original
close/capture, matching fresh CAT tuple, and fresh close/capture all succeed.

For `TM-D750 / 1.02 / K,2,1`, USB enumeration can precede CAT readiness.
The host policy permits at most six fresh identity attempts within its shared
budget. An endpoint absent for the whole enumeration window fails without a
fresh open; an enumerated endpoint still requires the complete CAT identity.

`post_exit_verification.outcome.status = "matched"` is reported separately from
the original probe outcome and never overwrites it. The selected endpoint and
CAT tuple identify a model and firmware, not the physical unit: paths can be
reassigned and this workflow has no USB serial identity. Fresh verification is
mandatory: there is no same-handle or capture-only success path.

## Diagnostic-only D-STAR probe

By default, `dstar probe` inspects a selected connection without initializing a modem,
connecting a reflector, or requesting mode, routing, configuration, or voice
changes. In Reflector Terminal on firmware 1.02, the probe returns MMDVM
protocol 1 with description `TM-D750 RTM1.00`, and a status reply in D-STAR
mode whose flags and buffer space are live modem state. That description is the
modem build string, not the CAT firmware version. Read the help without opening
a port:

```bash
cargo run -p tmd750-repl -- dstar probe --help
```

`--approve-live-test` is required before the probe opens any port; it performs
no routing check. Establish the radio's actual DV Gateway routing first and plan
any external mode changes and restoration: host bytes on a DV or DR route other
than Terminal can key the transmitter. This command neither reads nor changes
routing, and never enters or leaves Terminal Mode.

To run it, select the exact enumerated TM-D750 USB endpoint:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  dstar probe --approve-live-test --output terminal-probe-01
```

Only 9600 baud is admitted. The explicit `--port` must precede `dstar`; no
implicit endpoint, alias substitution, alternative port, or baud fallback is
used. `--output` must name a new directory whose parent exists. Omit it to
reserve a unique `captures/tmd750-dstar-probe-*` directory. Paths containing
spaces retain their original case and argument boundaries. On Unix, directories
are created with mode 0700 and files with 0600; other systems inherit permissions.
Non-Unix hosts require an explicit output directory with an existing parent.
Capture files are reserved before opening; an existing capture is never reused.

The diagnostic follows these boundaries:

1. Read CAT identity (`ID`, `FV`, `TY`). If it succeeds, read `GW`, report the
   observed identity and raw Gateway value, and close. No binary request follows
   any normal CAT reply, including one reporting Terminal on a different route.
2. Only a timed-out initial `ID` reply after exactly one completed write and
   zero received bytes permits binary queries on that same handle. A partial
   reply, failed write, later identity failure, or capture failure stops here.
3. Send one MMDVM `GET_VERSION`, followed only after an accepted protocol-1 or
   protocol-2 reply by one `GET_STATUS`. No runtime is started, and no periodic
   polling, modem configuration, mode command, retry, or reopen follows.
4. Close and drop the connection, synchronize its transcript, then write,
   flush, and synchronize the final report. Cleanup and capture failures remain
   separate from the protocol observations and prevent successful completion.

CAT has separate 1,500 ms write and reply budgets per query. The identity phase
can therefore consume six such budgets. Both binary exchanges share one
four-second absolute deadline, including their writes and every read; noise
cannot renew it. Close has a separate two-second bound. Synchronous OS opens,
file synchronization, and scheduling can exceed these host dispatch budgets;
the command does not promise a hard wall-clock duration.

Ctrl-C requests cooperative cancellation. An active CAT identity sequence or
binary exchange finishes or reaches its existing deadline before cleanup; the
next Gateway, version, or status query is suppressed at its boundary. A signal
during the final status exchange can leave successful observations in the
report, but cancellation still makes the command exit unsuccessfully. No CAT
recovery or persistent-mode exit is attempted on the gateway connection.

The format-1 `report.json` has `operation = "dstar_probe"`. Its typed `outcome`
distinguishes `cat_observed`, `mmdvm_observed`, cancellation, and failures.
Status failure preserves an accepted version reply; Gateway-query failure
preserves CAT identity. Endpoint metadata, duration limits (`secs`/`nanos`),
timestamps, cancellation, signal/close/synchronization failures, and transcript
completeness are explicit. `transcript.jsonl` records exact requested and
completed I/O separately, with timestamps. Required recording blocks further
protocol traffic after capture failure, but connection release is still tried.
Report-publication failure is returned to the caller, not recorded as success.
Malformed UTF-8 or embedded control characters in a version description are
rejected, as are unknown status mode bytes. Valid Unicode descriptions and
the codec's trailing padding remain supported; the raw transcript preserves
the original wire representation, including any reserved flag bits.

Exit zero means a complete CAT or a complete MMDVM diagnostic observation, with
clean close and synchronized file contents. Inspect `outcome.kind` to
distinguish them; CAT success is not MMDVM proof. A version or status reply
identifies neither the physical radio nor Reflector Terminal versus Access Point
mode, and MMDVM has no request correlation identifier, so a received status does
not say when the radio generated it. Keep the captures private and retain failed
attempts unchanged.

### Host-managed diagnostic lifecycle

Add `--manage-terminal` to enter Reflector Terminal for the diagnostic and
restore the captured settings afterward. This path is software-tested; the
complete automatic cycle is not validated on hardware. It does not initialize
a modem runtime, connect a reflector, relay voice, or change `dstar start`.

Both USB connectors must remain connected to the same intended radio: one for
the captured Gateway route and the other for CAT/MCP control. The selected
Gateway interface stops accepting CAT while active; it is not its own recovery
path. Bluetooth and same-interface escape commands are not supported here.
Provide a successful configuration backup and explicit endpoints:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.ModemPanel \
  dstar probe --manage-terminal --control-port /dev/cu.ControlMain \
  --backup backup-01/report.json --approve-live-test --output managed-probe-01
```

Replace both example paths with the actual enumerated endpoints. No endpoint
is substituted. Backup coverage, exact firmware/type, format, active PM, COM+AF,
captured Gateway route and known mode/subtype are checked before radio access.
The control endpoint must be independent of the selected modem route. Routing,
PM selection, MY callsigns and RPT fields are never changed. An Off-to-Terminal
plan changes only Gateway and, if necessary, the Reflector subtype; its inverse
retains the exact original page images, including the prior subtype.

Entry and restoration share one guarded exchange implementation. Each requires
fresh control identity/Gateway and equality of every complete format, PM,
routing and target page before any write. A changed page is journaled and
synchronized, then written once and read back whole. Equal pages are compare-only.
Acknowledged exit, clean close/drop, bounded identity-only CAT reacquisition
and fresh Gateway verification precede the next phase. A changed before-image
stops writes; there is no merge, blind rollback or retry of MCP programming.

The private report uses `operation = "managed_terminal_diagnostic"` and retains
entry, diagnostic and restoration results independently. Separate transcripts
cover every connection phase; `terminal-journal.jsonl` preserves the original
pages and write intents before dispatch. Managed capture currently requires
Unix private-file permissions and directory synchronization. Ctrl-C suppresses new diagnostics but
does not discard an existing restoration obligation. Uncertain programming or
failed cleanup/capture can prevent safe restoration; the report and journal
retain that obligation. Abrupt process termination cannot restore automatically.
Keep the captures of a failed run and do not rerun against stale images.

Success requires the MMDVM diagnostic, clean cleanup, and any owed restoration.
Restoration is verified by immediate whole-page readback followed by fresh CAT
identity/Gateway, not by another post-exit memory read or across a power cycle.

## Reflector Terminal Mode

`dstar start CALL [REFLECTOR]` validates its arguments and then returns
`ConfigurationError::UnsupportedProfile { profile: KenwoodTerminal }` before
enumerating or opening an endpoint, creating captures, writing Terminal pages,
or contacting a reflector. A complete `GET_VERSION` response identifies the
framing dialect; it does not establish a compatible Kenwood `SET_CONFIG`
payload. The public MMDVM V1/V2 layouts are not substituted for that missing
contract.

Read-only `dstar probe` remains available through its explicit USB and route
preconditions. Its optional `--manage-terminal` workflow performs guarded
entry, diagnostic queries and restoration without creating a modem runtime or
sending `SET_CONFIG`. When modem close fails, restoration remains owed and no
later control connection is opened. Keep its recovery journal and use the
reported connection and framing state before choosing another operation.

This application has no reflector network runtime, host-file loader or
automatic Bluetooth Terminal activation path. The shared model, MMDVM and
D-STAR libraries retain their independently checked APIs. The application
accepts startup syntax only to return the typed unsupported-profile refusal.

## Logging and output

Logging is off by default. `--trace` enables a separate trace file for the
session, including raw serial traffic. `--log-level error|warn|info|debug|trace`
selects less or more detail; `--trace` takes precedence. The actual log path is
printed at startup. Colliding session names get a numeric suffix; earlier logs
are never truncated.

| Platform | Log directory |
| --- | --- |
| macOS | `~/Library/Logs/tmd750-repl/` |
| Linux | `$XDG_STATE_HOME/tmd750-repl/`, or `~/.local/state/tmd750-repl/` |
| Windows | `%LOCALAPPDATA%\tmd750-repl\logs\` |

`RUST_LOG` independently enables diagnostic output on stderr, for example
`RUST_LOG=kenwood_tmd750=trace,kenwood_transport=trace` includes model diagnostics
and raw physical transport traffic. It does not enable a file log. Logs can contain
callsigns, addresses, and radio traffic; review them before sharing. There is
no automatic retention policy, so remove old logs when you no longer need them.

Terminal output is plain, line-oriented text without color or cursor-based
status displays. `--timestamps` prefixes each output line with a UTC clock.
Prose wraps at 80 characters including the prefix; individual whitespace-free
tokens remain intact for copying. Command history is available within
the session but is not persisted.

## Commands

```text
help
identity
status
mode [a|b]
mode [a|b] fm|dv|am|nfm
dv [a|b]
fm [a|b]
normal [a|b]
gateway
terminal
serial | clock
freq [a|b] [MHz]
channel [a|b]
power [a|b] [high|medium|low]
tuning [a|b] [vfo|memory|call|dr]
squelch [a|b] [0-31]
smeter [a|b] | busy [a|b]
att [a|b] [on|off]
step [a|b] [kHz]
up [a|b] | down [a|b]
am-cut [3.0|4.5|6.0|7.5]
current [a|b] | recall [a|b] ADDRESS | memory ADDRESS
store [a|b] ADDRESS | clear ADDRESS
bands [CTRL PTT]
display [dual|single]
slot [1-6]
callsign 1-6 [CALL [MEMO]|none]
aprs-callsign [CALL[-SSID]]
backlight [0-3]
position [gps|1-5]
data-rate [1200|9600] | beacon [manual|ptt|auto|smart]
tnc [a|b] [off|aprs|kiss]
vox [on|off]
vox-delay [ms] | vox-gain [0-9]
gps [on|off on|off]
sentences [gga,gll,gsa,gsv,rmc,vtg|none]
bluetooth [on|off]
tx SECONDS
rx
send-beacon
power-off
quit
```

Band A is the default when a band is omitted. A word without a value reads
the setting; a word with a value writes it with the library's echo and
readback and prints the confirmed value. Words and values match in any letter
case, except callsign and memo text, which is sent exactly as typed.
`callsign N CALL [MEMO]` stores MY slot N of the ordinary DV MY list, the
list `slot` selects; `callsign N none` clears it, so `none` in any letter
case is never stored as a callsign. CALL and MEMO are single words of at
most eight and four characters without commas. That list is not
the DV Gateway MY list of Terminal mode, which only `mcp text set
dv-gateway-my-callsign-1` and `mcp menu apply` write. `aprs-callsign` reads or
stores the APRS My Callsign, an uppercase base of up to six characters with
an optional `-SSID` from 1 to 15, which is the menu field `aprs.MyCallsign`;
storing it does not transmit. On firmware 1.02 over the operation-panel USB
endpoint, `callsign 1 KQ4NIT TEST`, `callsign 6 N0CALL SIX`, `slot 2` and
`aprs-callsign KQ4NIT-9` each changed exactly their stored bytes in a fresh
`mcp backup`, and `callsign N none`, `slot 1` and `aprs-callsign NOCALL`
restored every byte. `dv` selects the ordinary
D-STAR RF operating mode; it does not turn on the separate persistent Terminal
Mode. `tuning dr` selects the D-STAR repeater list. `up` and `down` step the
control band by its tuning step and refuse the other band. `store` writes the
band's current channel record, the one `channel` reads, to a stored memory
channel without scan lockout and requires the echo and readback to match it;
storing keeps the channel's name, which `mcp text set --channel` or
`mcp channel-names apply` writes. `clear` empties a stored memory channel,
erasing its name, and requires the readback to answer `N`. On
firmware 1.02 over the operation-panel USB endpoint, `store` wrote twenty
channels from both bands between 108 and 523 MHz, each verified by echo and
readback and then cleared. `freq` outside a band's ranges is not refused: the
radio tunes a nearby in-range frequency and the command reports the differing
echo. The `terminal` command describes CAT state checks and the independent
control connection required by managed diagnostics; it writes no setting.

`tx SECONDS` keys the PTT band, the second band of `bands`, on its current
frequency, mode and power level for 0.001 to 300 seconds with up to three
decimals, then sends `RX`; Ctrl-C during the hold releases at once. There is
no unbounded key-up. The radio answers `TX` with `TX 1` and `RX` with `RX`.
When `TX` fails after it may have reached the radio, `tx` sends `RX` at once
and reports the failure; when the radio does not acknowledge an `RX`, the
error says the transmitter may still be keyed and ends with `send rx`. `rx`
releases the transmitter even after a failed exchange and, over native
Bluetooth, needs no fresh Gateway reply. `tnc [a|b] off|aprs|kiss` selects the TNC mode and its data
band with echo and readback. APRS mode beacons by itself under the `auto` and
`smart` beacon methods, so select `beacon manual` first to transmit only on
`send-beacon`, which the radio refuses while the TNC is off. KISS mode hands
the TNC to the interface Menu 984 selects; selected over the operation-panel
endpoint while Menu 984 selected the main unit, it re-enumerated that endpoint
before replying, so the command fails with a transport error and the
connection ends. `vox on` lets microphone audio key the transmitter.
`power-off` sends `PS 0` and ends the session; its reply has not been read
from a radio. The operator is responsible for the license conditions of every
transmission.

Startup-only workflows are described above:

- `dstar start CALL [REFLECTOR]` refuses the unverified Kenwood modem
  configuration before endpoint access.
- `dstar probe` captures bounded CAT or MMDVM diagnostics; `--manage-terminal`
  adds guarded entry/restoration through an independent USB control endpoint.
- `mcp probe` and `mcp backup` use dedicated radio connections and private
  capture directories.
- `mcp reentry-probe` runs one read-only MCP session pair with Gateway Off and
  passive macOS serial-registry observation.
- `mcp text list|show|preview` and `mcp terminal preflight` work offline without
  enumerating or opening a radio endpoint.
- `mcp menu list|describe|show|preview` discovers and inspects registered fields
  offline. `mcp menu apply` uses the ordinary scalar policy, an explicit
  endpoint (`--port`, or `--bluetooth` with a native backup of that radio), a
  current backup, and `--apply`; it requires immediate readback and fresh CAT.
- `mcp text set` requires the same explicit endpoint and current backup, the
  expected text, and `--apply`; it changes PM1's name, PM Off MY1 or one
  channel's name (`--channel`) through dedicated connections.

These workflows cannot run inside the interactive CAT prompt. Quit the prompt
and invoke the desired workflow in a new process; use its `--help` for options.

Supply a CAT command after the options to run it once and exit:

```bash
cargo run -p tmd750-repl -- status
cargo run -p tmd750-repl -- dv a
cargo run -p tmd750-repl -- normal a
```

Use `--help` for startup options or `help` for the command list. `help` and
`terminal` can run without opening a radio connection.
