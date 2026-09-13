# TM-D750 REPL

`tmd750-repl` is a plain-text USB shell with native macOS Bluetooth D-STAR and diagnostics
for the Kenwood TM-D750. It is designed
to work well with a screen reader. It supports normal CAT control plus an
experimental, protocol-gated D-STAR Reflector Terminal Mode path.

The REPL can select FM or
D-STAR DV on either band, and every selection requires an immediate readback
from the radio. It reads persistent DV Gateway state, naming the observed
values Off and Terminal. Ordinary CAT does not change that setting. Automatic
Bluetooth `dstar start` captures and verifies its Terminal/routing changes,
then restores its original settings through independent USB control on shutdown.
The diagnostic's explicit `--manage-terminal` option remains a separate
diagnostic-only entry/restoration workflow.
It does not expose arbitrary CAT commands or arbitrary MCP memory writes.
The schema-driven `mcp menu` commands discover, inspect, and preview registered
settings. Ordinary scalar updates are software-admitted on firmware 1.02 with
complete captured-page guards, immediate full-page readback, and fresh CAT
verification. This is not hardware qualification of every setting.
Gateway, transport, and automatic-transmission changes need separate lifecycle
support and cannot be applied through the ordinary menu setter. Managed
diagnostics remain software-tested. Automatic Bluetooth startup and restoration
have been observed on the firmware-1.02 configuration described under
[Reflector Terminal Mode](#reflector-terminal-mode). The older PM1 and MY1
setters and fixed qualification experiments remain separate workflows.
The manifest's declared firmware label 1.00 is a project compatibility gate,
not an extracted vendor maximum-version restriction.

## Run

Connect USB to the radio, leave it on its normal screen, and run:

```bash
cargo run -p tmd750-repl
```

Auto-discovery recognizes the main-unit and control-panel USB serial
endpoints. If more than one is connected, the REPL refuses to guess and
requires an explicit endpoint:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101
```

The default baud rate is the hardware-validated 9600 baud. Use `--baud` only
when testing a deliberately different configuration.

The TM-D750 library owns USB discovery and the RTS/CTS, DTR, and RTS preset;
`kenwood-transport` supplies shared physical serial I/O and transport contracts.
Endpoint selection and post-MCP readiness remain explicit REPL policies, not
automatic behavior in the shared transport. The experimental D-STAR runtime
comes directly from `mmdvm::dstar`, with no TH-D75 library dependency. The REPL
owns TM-D750 protocol admission, the selected connection, and final serial
close; the shared runtime owns MMDVM D-STAR processing. This separation does
not qualify additional TM-D750 protocols or lifecycle operations.

Read-only CAT status has been hardware-validated on both main-unit and
control-panel USB with firmware 1.02. USB identity alone remains a candidate
filter, not protocol qualification or proof that a radio is currently ready.
The transport does not automatically reopen after a USB disconnect: serial
pathnames can be reassigned. Reconnect the intended radio and select its current
endpoint for a new session.

### Native Bluetooth diagnostics on macOS

Select the radio's exact paired Bluetooth address, not its display name or
`/dev/cu.*` alias. Replace the example address with your radio's address:

```bash
cargo run -p tmd750-repl -- --bluetooth 01:23:45:67:89:AB status
cargo run -p tmd750-repl -- --bluetooth 01:23:45:67:89:AB \
  mcp probe --output bluetooth-probe-01
```

Native startup commands currently support `identity` (or `id`), `status`,
`gateway`, and the fixed read-only `mcp probe`. Each captures its observations
and cleanup. Omitting the probe's `--output` creates a new private directory
under `captures`; CAT diagnostics use that default destination. Programming
temporarily interrupts normal radio operation, even without settings writes.
General native backup/settings commands and interactive CAT are not supported
by this diagnostic path. Automatic `dstar start` has its own narrowly guarded
Bluetooth lifecycle, described below. Offline MCP inspection remains available.

The native MCP probe requires `TM-D750 / 1.02 / K,2,1` and a fresh Gateway Off
reply. It reads exactly two fixed fragments, totaling 295 bytes, then requires
the exit acknowledgment. The original connection is retained without further
protocol traffic for five seconds, then closed and dropped. Only complete
read/exit evidence, a clean original close and complete captures admit recovery.
Recovery reopens the exact address and the RFCOMM channel from that original
successful connection, then verifies the original CAT identity and Gateway Off.
The wait is host scheduling policy, not proof that firmware is ready.

Each opening phase permits at most two attempts, separated by one second, for
eligible native opening failures. A retry uses a new helper after the previous
helper has been reaped. A typed opening failure may retain unconfirmed native
cleanup independently; retrying it does not establish cancellation of an OS
operation. Cancellation, helper launch/framing errors, an invalid or changed
endpoint, incomplete capture, and a late successful owner's failed cleanup
prevent another attempt. Neither MCP nor a CAT exchange is retried. In
particular, a changed identity or Gateway reply stops recovery.

Format-2 native reports retain every opening attempt, including a failed attempt
before eventual success, and distinguish initial SDP discovery from fixed-channel
recovery. Earlier format-1 captures retain their original single-attempt policy;
they are not reinterpreted as evidence for the new workflow. This is ordinary
MCP-to-CAT recovery, not the separate Terminal-to-MMDVM transition lifecycle.

Opening failures identify the observed host stage, such as an SDP completion
deadline or an RFCOMM opening deadline. The command prints each failed attempt
and retains it in the report, together with any independent cleanup failure.
A stage identifies where progress stopped, not the underlying cause in the
operating system or radio.

The initial opening performs a fresh Serial Port Profile service query and
records the selected address and RFCOMM channel from that same open. Recovery
pins that channel for this workflow, using the fixed-channel baseband wakeup
before RFCOMM opening. The channel is not a permanent model constant. It does not
guess a channel from another radio model, auto-select by display name, or fall
back to the operating system's serial alias. Do not combine `--bluetooth` with
`--port` or `--baud`. Packaged installations can supply a trusted executable
with `--bluetooth-helper /absolute/path/to/helper`; ordinary builds include it.

Normal SDP-selected native CAT and fixed MCP read/exit have responded on a
firmware 1.02 radio while the OS serial alias timed out. This does not establish the
cause of the serial timeout, a requirement for manual connection, or cold-start
connection reliability. One native MCP trial completed both reads and
acknowledged exit with a clean original close, but the fresh SPP opening timed
out. The five-second retained-owner wait and selected-channel retry lifecycle
are software-tested, not a completed hardware qualification of ordinary
MCP-to-CAT recovery. The separately observed
[automatic Terminal-to-MMDVM cycle](#reflector-terminal-mode) does not establish
cold-start reliability. A separate cold attempt changed the Mac's reported
connection state to connected without receiving SDP completion; an SDP-only
query also timed out. Neither a connected flag nor a cached service record
proves a usable SPP session. Keep other applications off the selected radio
connection while running diagnostics.

## Configuration backup and PC text entry

Read the standard configuration through a dedicated connection:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp backup --output backup-01
```

The directory must be new and its parent must exist. Omitting `--output`
reserves a unique private directory under `captures`. The command reads the
official standard configuration schedule: 1,138 pages containing 289,962 bytes,
including all six Programmable-Memory slots. It excludes the custom startup
bitmap and unclassified regions. It sends no settings writes or RF requests.
Programming temporarily interrupts normal radio operation.

The earlier single-attempt read/exit/reconnect sequence was bench-validated on
main-unit USB with firmware 1.02 on September 7, 2026. Every captured page matched
its raw wire response, both connections closed cleanly, and fresh CAT identity
matched. Two panel-USB backups on September 12, 2026 also completed bounded CAT
reacquisition. Their first two and three fresh ID attempts, respectively, were
silent and closed cleanly; the next handle returned matching ID/FV/TY. All
1,138 saved pages in each capture matched the raw responses. These observations
qualify those backup sequences, not a universal readiness bound or settings
writes.

The command closes and drops the original handle after the exit ACK, waits two
seconds, then uses [bounded CAT reacquisition](#bounded-cat-reacquisition).
One sixty-second dispatch window covers passive enumeration, up to four fresh
connections, and two-second retry waits. Every open requires the original path
and unchanged recognized VID/PID. Only a completely silent initial `ID` reply
timeout permits another connection; partial replies, identity mismatches, and
open, write, close, or capture failures stop recovery. This retries only
post-exit identity checks, never MCP entry, configuration reads, or exit. An
explicit `--port` is mandatory. Cancellation finishes the current exchange;
incomplete framing prohibits speculative exit or recovery commands.

`report.json` uses format version 4 with `operation = "configuration_backup"`.
Its `backup.segments` retain every fully acknowledged page even if a later page,
cleanup, or fresh verification fails. Addresses and lengths are explicit; unread
gaps are absent. Original and fresh transcripts remain separate.
`post_exit_verification` records policy limits, elapsed readiness time, every
entry in `attempts`, the aggregate transcript result, and the final outcome.
Earlier silent timeouts remain recorded when a later identity check succeeds.
A successful exit requires the entire page schedule, acknowledged MCP exit,
clean closes and complete captures for every connection, matching fresh identity,
and a written, flushed, synchronized report.

Offline inspection and update planning accept complete successful format-4
backups and historical format-3 backups with their original single-attempt
evidence. The loader checks the evidence shape for the declared format; it does
not upgrade reports, reinterpret failed captures as successful, or establish
the radio's current state. Reports must be regular files no larger than 32 MiB
and contain every standard page in order with exact addresses and lengths.

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
binary fields. These are software-policy classifications, not bench-test counts
or a claim of complete MCP application parity. Binary fields have no scalar
preview or setter. Unknown stored numeric values remain visible when read.
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

To leave an ordinary value configured, select the main-unit USB endpoint,
provide a successful current backup from that radio, and explicitly apply:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp menu apply --backup backup-01/report.json --apply \
  --output menu-update-01 pm.PmName2 "Base Camp"
```

The live command currently accepts one assignment. The library plan supports
multiple assignments across global and per-slot fields in one session. Live
admission requires `TM-D750 / 1.02 / K,2,1`, main-unit USB at 9600 baud, memory
format zero, a valid active PM selection, and Gateway Off. Complete format,
PM-control, active-Gateway, and target pages must all match fresh reads before
the first write. A mismatch aborts; the command does not silently merge newer
radio settings into the old backup. Equal target pages are compared without
writes. Unrelated bytes and neighboring bits remain exact.

Each changed page requires a synchronized raw transcript and a private journal
containing both complete images before dispatch. There is one W/ACK and one
immediate complete-page readback per changed page. After acknowledged MCP exit,
the original connection closes and drops; one fresh connection must confirm the
same CAT identity and Gateway Off, then close cleanly. This is **two handles and
one MCP session**, not a persistence test across MCP re-entry or power cycling.
It does not qualify a fixed wait as firmware readiness or prove physical-unit
identity from a reused USB pathname.

Keep the same radio connected and close other radio applications. Ctrl-C can
cancel before the first write intent. Afterward, the approved batch and safe
verification finish unless a protocol, capture, journal, or cleanup failure
prevents them. Multi-page updates are not firmware-atomic. Incomplete exchanges
prohibit speculative exit, retry, reconnect, or automatic rollback. Earlier
verified writes remain reported if a later page fails. No RF command is sent;
this is not a guarantee that independently configured radio functions are RF
silent. Programming interrupts normal operation.

The new private directory contains a format-1 `report.json` with
`operation = "menu_update"`, `menu-journal.jsonl`, `transcript.jsonl`, and
`post-exit-transcript.jsonl`. Retain them on failure; do not restore stale pages
blindly. Journal preparation retains every complete before/after image, including
unchanged guards. Report `compared_pages` describes a successful whole batch;
partial comparisons remain in the raw transcript. `possible_pages` includes
verified writes as well as uncertain dispatches. Successful output requires
durable final evidence too. Live updates require Unix private-file and directory
synchronization support. An update report is not a new configuration backup;
refresh the backup before the next edit.

The generic workflow is tested with mock transports, not yet run on hardware.
The narrow experiments below remain the available field-level bench evidence.
The ordinary policy is separate from the unchanged legacy raw-page schema gate;
it does not enable arbitrary addresses or firmware overrides.

### Dedicated PM1 setter with re-entry verification

The `pm-name-1` form of `mcp text set` changes the global PM1 label. Start with a successful
configuration backup from the connected radio, then supply the expected current
name, the replacement name, and explicit write approval:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp text set --backup backup-01/report.json --expect PM1 --apply \
  --output pm1-update-01 pm-name-1 "Home"
```

This command leaves `Home` in place; it does not restore `PM1`. Names must be
1 through 16 printable ASCII bytes. Case and spaces are preserved; input is
never truncated. Only main-unit USB at 9600 baud and the TM-D750 / firmware
1.02 / type K,2,1 identity are accepted. The separate MY1 form is described below;
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
and a separate fresh CAT identity check with a clean close. This is four
connections in total. It requests no other setting change or RF transmission.

Keep the same radio connected and close other radio applications. A pathname
and public identity tuple cannot prove physical-unit continuity. Ctrl-C can
cancel before write intent; afterward the command finishes the remaining safe
verification steps. A real failure stops the workflow. Uncertain framing does
not permit speculative exit, retries, or automatic rollback. Retain the journal
if a write may have occurred; do not blindly restore an old page. If programming
exit is unconfirmed, fully power-cycle before reconnecting. This instruction
concerns an uncertain MCP state, not silence after an acknowledged exit. A power
cycle does not establish which name is stored. This workflow requires Unix
private-file and directory synchronization support.

The output directory must be new. It contains a format-5 `report.json` with
`operation = "pm1_name_update"`, `update-journal.jsonl`, and separate transcripts
for both MCP sessions and both fresh CAT checks. The final status distinguishes
`not_written`, `possibly_changed`, and `verified_across_sessions`. These files
contain private settings. An update report is not a configuration backup:
**take a new full backup before the next edit**, since the old PM1 page is stale.

The underlying fixed PM1 rename/restore mechanism was live-tested on September
11, 2026, as recorded below. This configurable, leave-in-place command has been
tested with mock transports, not run on hardware. Neither result establishes
general keyboard/HID input, other writable text fields, independent display
rendering, power-cycle persistence, or automatic Terminal Mode.

### Dedicated PM Off MY1 setter with re-entry verification

The `dstar-my-callsign-1` form of `mcp text set` leaves a requested DV Gateway
MY1 value configured in PM Off. For a currently empty MY1, use an explicitly
empty expected value:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp text set --backup backup-01/report.json --expect "" --apply \
  --output my1-update-01 dstar-my-callsign-1 KQ4NIT
```

To replace existing text, supply that exact text to `--expect`. Callsigns must
contain one to eight uppercase ASCII letters, digits, or spaces, including at
least one letter or digit. Spaces remain exact. The command neither normalizes
input nor inserts a suffix; its eight-byte storage uses NUL padding. An empty
desired value, slash, lowercase input, non-ASCII text, or no-op is refused before
radio access. This validates storage text, not ownership or Terminal acceptance.

Both the backup and fresh reads must show PM Off, Gateway Off, and MY1 selected.
Main-unit USB at 9600 baud and exact `TM-D750 / 1.02 / K,2,1` identity are
required. Before writing, the entire fresh target and control pages must match
the immutable backup. Verification compares the entire desired target page and
the unchanged control page. Only MY1's eight bytes can change; its memo, other MY entries, Gateway
mode, selection, and every unrelated byte are preserved. No other PM slot,
routing change, Gateway activation, RF command, or automatic rollback is allowed.

The two-session lifecycle shares PM1's capture and journal protections. MY1
additionally requires fresh CAT Gateway Off before both MCP entries and after
both exits. Original and fresh connections must close cleanly, and complete
evidence must be synchronized before the next session. Cancellation before
intent prevents new connections; after possible dispatch, safe verification
finishes, while any failed requirement stops without retry or stale restoration.

The new private directory contains a format-6 report with
`operation = "my1_callsign_update"`, the immutable `update-journal.jsonl`, and
separate original/fresh transcripts for both sessions. Keep these files private.
Take a new full configuration backup before another edit; an update report is
not a backup. The same Unix private-file and directory-sync requirements apply.

This configurable leave-in-place workflow is software-tested, not yet run on
hardware. The fixed empty-to-`KQ4NIT`-and-restoration experiment below is its
bounded field-layout evidence. Neither operation enables automatic Terminal
switching or establishes power-cycle persistence or complete reflector operation.

### Fixed PM1 qualification experiment

`mcp pm1-trial` is a separately approved bench experiment, not a general text
editor or a firmware-wide compatibility override. It is limited to main-unit
USB at 9600 baud and the TM-D750 / firmware 1.02 / type K,2,1 tuple. Independently
read PM1's name on the radio without recalling a profile, then explicitly
approve a temporary rename to `PC TEXT TEST` and restoration of the original:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp pm1-trial --backup backup-01/report.json --confirmed-name PM1 \
  --approve-live-test --output pm1-trial-01
```

The runner reserves private evidence files before opening USB. It requires
exact agreement with the backup's complete PM1 page, synchronizes a recovery
journal before each write, compares full immediate readbacks, and checks both
the temporary name and restored page across exit/re-entry in three separate MCP
sessions. Each exit requires a clean old-handle close and one independently
captured fresh CAT identity check. No other field or RF command is requested.

Keep the same radio connected throughout. A path and public identity tuple
cannot prove physical-unit continuity. Ctrl-C can cancel before a write intent;
afterward it does not abandon restoration. Incomplete framing, capture failure,
page drift, or failed identity/close verification stops further commands and
retains the recovery evidence. Do not retry or restore a stale page blindly.
Only the complete three-session proof reports restoration verified across MCP
exit/re-entry. It does not independently prove a full-radio reboot or
persistence across a power cycle. This
experimental runner requires Unix private-file and directory synchronization
support; it fails closed on unsupported platforms.

`report.json` uses format version 4 and `operation = "pm1_name_trial"`.
`trial-journal.jsonl` retains both complete pages and durable write intents;
each MCP session and fresh CAT verification has a separate transcript. These
artifacts can contain private settings.

This fixed experiment succeeded on main-unit USB with firmware 1.02 and type
K,2,1 on September 11, 2026. The original `PM1` label changed to `PC TEXT TEST`,
then the complete original page was restored and verified in a third MCP
session. Both writes were acknowledged and read back exactly; all six
connections closed cleanly with matching identities. This demonstrates PC
text-setting access through MCP for this PM1 operation, not a general keyboard
interface, other text fields, power-cycle persistence, or automatic Terminal
Mode. General firmware-1.02 settings writes remain gated.

### Fixed MY1 qualification experiment

`mcp my1-trial` is a separate, explicitly approved keyboard-entry experiment.
It replaces an empty DV Gateway MY1 with `KQ4NIT` in PM Off, verifies the complete
page, then restores the exact original page. The command does not retain the
callsign or activate Terminal Mode:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp my1-trial --backup backup-01/report.json \
  --approve-live-test --output my1-trial-01
```

The target is fixed: main-unit USB at 9600 baud, firmware 1.02, type K,2,1,
PM Off, MY1 selected and initially empty, with DV Gateway Off. Arbitrary
callsigns, fields, addresses, slots, and force overrides are not accepted.
Before each MCP entry, the runner requires a fresh CAT Gateway-Off observation.
It then checks the format, complete PM-control page, and complete target page
against the captured baseline. Only the eight MY1 storage bytes can differ;
its memo, other MY entries, gateway settings, and all other page bytes remain
unchanged. The six letters are NUL-padded, not space-padded or given a suffix.

The shared three-session workflow verifies the temporary page after exit and
re-entry, restores the full original page, and verifies restoration in a third
read-only MCP session. Every write requires a synchronized private recovery
journal containing both target pages and the unchanged control-page baseline.
The existing cancellation and uncertain-exchange rules above also apply.
The format-4 report has operation `my1_callsign_trial`; it is not a full backup.
An empty captured MY1 is not claimed as independently observed display text.

This fixed experiment was bench-validated on main-unit USB, firmware 1.02,
type K,2,1 on September 11, 2026. Independent raw-transcript auditing verified
both writes and all 11 read segments, including the temporary callsign in the
second session and exact restoration in the third. Gateway was observed Off
before every entry, all six closes succeeded, and no Gateway setter, PM recall,
or RF command was sent. A fresh full backup matched the pre-trial backup across
all 1,138 captured pages / 289,962 bytes; final CAT status reported Gateway Off.
MY1 was restored to empty.

The runner remains experimental. This result does not independently prove
display rendering, power-cycle persistence, callsign acceptance for Terminal
operation, or automatic entry/exit. Keep the evidence private and take a new
complete backup after any separately approved trial to compare every covered
byte. Generic firmware-1.02 schema writes remain disabled.

### Experimental Terminal exit

`mcp terminal-exit-trial` is a separately approved, fixed Terminal-to-Off
experiment, not automatic D-STAR setup or a generally qualified Gateway setter:

```bash
cargo run -p tmd750-repl -- mcp terminal-exit-trial --help
```

Execution requires an explicit main-unit `--port` before `mcp`, 9600 baud,
`--backup REPORT` from a complete successful Gateway-Off backup, and
`--approve-live-test`. The admitted configuration is TM-D750 / firmware 1.02 /
type `K,2,1`, PM Off, Reflector subtype, COM+AF USB, and panel USB Gateway routing.
No independent display confirmation is required: fresh CAT verifies the state.

The runner reads format, the full PM-control and routing pages, and the full
Gateway page. All bytes must match the saved baseline, except the Gateway-mode
byte must now be Terminal. Any other difference refuses the write. Only after
the exact scope and intent are synchronized to private storage may one whole
page write restore that byte to Off. Callsigns and all unrelated bytes are
preserved. Immediate full-page readback and a second read-only MCP session
must match; after both exits, newly opened connections must return matching
identity and Gateway Off, close, and synchronize their captures.

`--output NEW_DIRECTORY` reserves private evidence without overwriting an
existing capture; otherwise a unique directory is created under `captures/`.
The format-1 report uses operation `terminal_to_off_trial` and the separate
`terminal-exit-journal.jsonl` retains scope, intent, session evidence, and final
status. Neither file is a full backup. Keep them private and compare a fresh
full configuration backup after any approved live trial.

Original MCP transcripts also timestamp the requested open and its outcome.
The request is synchronized before opening; a successful opening must be
recorded and synchronized before CAT traffic. Recording failure after opening
permits only close/drop. These boundaries improve evidence retention; they are
not a qualified delay or readiness check. Console summaries separate historical
write/readback/Off observations from the trial's overall completion status.

Ctrl-C can cancel before intent; afterward it cannot abandon owed verification.
Failures stop without retry or speculative rollback and retain possible-change
status. Hardware qualification remains incomplete: one firmware-1.02 trial
completed the sole write, exact immediate readback, and fresh CAT Off, then
lost USB while entering the second MCP session. A later separate full backup
matched all three guarded pages; ten bytes elsewhere differed from the
historical baseline, with cause unestablished. The failed trial was not retried
or reclassified as complete. Fresh CAT Off alone does not establish immediate
MCP re-entry readiness.
It sends no RF, reflector, PM-recall, routing, or callsign commands and does not
widen the general firmware write gate. Manual USB `dstar stop` leaves Terminal
Mode unchanged. Automatic Bluetooth startup separately restores changes made
by that session through its independent USB control path.

### Offline text inspection

`mcp text list`, `show`, and `preview` work entirely offline, without enumerating
or opening USB:

```bash
cargo run -p tmd750-repl -- mcp text list
cargo run -p tmd750-repl -- mcp text show \
  --backup backup-01/report.json --interpret-unqualified pm-name-1
cargo run -p tmd750-repl -- mcp text preview \
  --backup backup-01/report.json --interpret-unqualified \
  --output pm-name-preview.json pm-name-1 "Local repeaters"
```

Supported fields include five PM names, six D-STAR MY callsigns and their memos,
five D-STAR messages, and the power-on message. Per-slot fields require an
explicit `--slot 0` through `--slot 5`; global PM names reject `--slot`.
`mcp text list` reports each stable key, scope, encoding, and encoded-byte limit.
Input is never silently truncated or case-folded, and control characters are
rejected. Field encoding is not a claim of radio-side callsign syntax validation.

The generated manifest carries the declared firmware label 1.00. This label
is our compatibility gate, not an extracted vendor maximum-version rule.
Firmware 1.02 therefore requires
`--interpret-unqualified` for offline interpretation. This opt-in is retained
in the preview alongside the original firmware identity; it does not enable
radio writes or establish that the layout is correct. The reader requires a
complete successful configuration capture and verifies every requested field
byte was captured before decoding it.

Preview prints the existing and proposed text without changing the source.
An optional new private JSON file records provenance, qualification, before/after
text, and exact masked page changes. It explicitly records `radio_applied: false`.
Existing files are never overwritten. A preview does not authorize or apply
an update. The dedicated PM1 and MY1 setters provide live text writes; ordinary
scalar settings use the separate menu apply workflow. There is no generic
keyboard injection or automatic Terminal Mode settings write.

## Offline Terminal settings preflight

Inspect the Terminal-related settings in a successful configuration backup:

```bash
cargo run -p tmd750-repl -- mcp terminal preflight \
  --backup backup-01/report.json --slot 0 --interface main-usb \
  --interpret-unqualified
```

`--slot` explicitly selects PM 0 (PM Off) or PM 1 through 5;
`--interface` selects `main-usb` or `panel-usb`. These are requested settings to
compare, not instructions to switch PM or connect a USB port. The report
distinguishes the requested slot from the active PM recorded in the backup,
and the requested interface from the captured DV Gateway route. It also
reports the selected MY callsign and whether it is empty, USB function,
Terminal subtype, DV Gateway mode, and the stored RPT1/RPT2 text.

All observations describe the historical capture, not the radio's current
state. Firmware 1.02 requires `--interpret-unqualified`; the interpretation
does not validate the schema. The command neither normalizes values nor
guesses missing callsigns or identification suffixes. A suffix finding requests
operator review; it is not a radio-side acceptance test. Findings are not proof
of activation readiness, MMDVM availability, Internet access, or a working
reflector connection, even when the captured settings agree with the request.

This command does not enumerate or open radio interfaces, activate Terminal
Mode, generate write patches, or change the backup or radio. Use it to review
captured configuration before a separately controlled setup or qualification.

## Compare configuration captures

Compare two successful configuration-backup reports, current format 4 or
historical format 3:

```bash
cargo run -p tmd750-repl -- mcp terminal compare \
  --before backup-01/report.json --after backup-02/report.json \
  --slot 0 --interface main-usb --interpret-unqualified
```

Every standard configuration byte is compared: 1,138 pages and 289,962 captured
bytes across all six PM slots. Unread gaps and the startup bitmap are excluded.
`--slot` selects only the two offline preflights, not a comparison filter;
`--interface` supplies their intended USB route without selecting an endpoint.
Firmware 1.02 requires `--interpret-unqualified`.

The finite software-layout catalog labels candidate changes in 116 fields
covering 560 bytes: the global format and active-PM selectors, plus menus 980,
986, 650, 651, 670, 671, and 672 in every PM slot, including all six MY callsigns
and their memos. Changes outside that catalog remain in the comparison and
are counted explicitly; absence from the catalog does not prove a setting is
unrelated to Terminal operation. Unknown or invalid values encountered by either
selected-slot preflight are recorded as failures without discarding the raw
differences. Memos, unselected MY entries, and other slots receive raw-byte
attribution, not semantic validation.
An otherwise successful comparison exits successfully even if either preflight
fails; the console and private report explicitly retain that failure. Successful
comparison does not mean either configuration is valid or activation-ready.

Console output shows counts, page addresses, and candidate field keys, but no
captured text or byte values, including when a preflight fails. Add
`--output terminal-comparison-01.json` to save exact differences, source paths
and identities, interpreted selected-slot
text, and detailed preflight errors. The file must be new and its parent
directory must already exist. Unix creation requests mode `0600`, subject to
the process umask; other platforms use their default or inherited permissions.
Choose a private directory and keep the report private. Its operation is
`terminal_configuration_comparison`; it is not a backup or a write plan.

Both inputs must pass coverage and completion checks and have identical model,
firmware, and full radio-type identities. These are structural checks, not
report authentication or an independent audit of the raw transcripts. Matching
identities do not prove the same physical radio, and before/after order is
caller supplied, not verified chronology. A difference establishes neither
causality nor hardware-layout compatibility. No USB enumeration, radio or RF
access, source-file changes, setting writes, or write-gate changes occur.

For subsequent controlled hardware qualification, compare one intentional
preparation-field change at a time with DV Gateway Off initially. PM recall is
a separate state-changing operation, not a read-only check. Enabling Menu 650
may remove CAT/MCP access on the selected interface, so do not assume that
activation can be followed by another backup on that port. Protocol handoff,
return to CAT, and restoration need separate qualification.

## Read-only MCP re-entry control

`mcp reentry-probe` is a separately approved diagnostic, not a Terminal Mode
setter. It requires macOS, an explicit main-unit USB endpoint at 9600 baud,
and `--approve-live-test` for two programming-mode interruptions:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp reentry-probe --approve-live-test --output reentry-01
```

Each session requires exact identity `TM-D750 / 1.02 / K,2,1` and fresh `GW 0`
before entry. It reads only `8..48` and `327681..327936`, acknowledges exit,
closes and drops the old handle, then uses the existing passive rediscovery
policy for one fresh identity/Gateway-Off check and close. Session 1's complete,
synchronized evidence is required before session 2 opens. There are no settings
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
then closes without starting another connection. That cancelled experiment is
incomplete. Uncertain entry, read, ACK, or exit stops active commands; only
close/drop follows, with up to 20 seconds of passive observation when not
cancelled. No speculative exit or recovery request is sent.

Success requires both read sessions, both fresh CAT-Off checks, every close,
complete synchronized captures, and a joined successful observer. A successful
pair is one measured sequence, not a universal readiness bound or qualification
of automatic Terminal switching. Reading 295 distinct bytes twice is not a full
configuration backup or proof that all settings are unchanged.

One approved firmware 1.02 main-unit USB pair completed on 2026-09-12 UTC.
Both MCP read/exit sequences and fresh CAT checks succeeded, with four observed
`GW 0` replies and four clean closes. Each session's 295 captured bytes matched
the other session and the corresponding bytes in the preceding full backup.
Raw-traffic audits found no settings writes, fill commands, retries, or RF commands.

The successful second entry was requested 28.111 ms after fresh CAT close and
12.549759 s after the first exit ACK. The earlier failed Terminal-to-Off trial
measured 17.595 ms and 11.428353 s, respectively. Different preceding operations and
timing prevent attributing success to a delay. This result does not repair or
reclassify that failed trial, or qualify automatic Terminal exit/restoration.

## Fixed MCP probe

The startup-only `mcp probe` command captures a small, fixed protocol check. It
reads CAT identity, enters MCP programming mode, reads 40 bytes at address 8
and 255 bytes at address 327681, then requires MCP exit, releases the original
handle, and uses bounded, identity-only CAT reacquisition. It sends
no settings writes, fill commands, arbitrary memory requests,
or RF transmission requests. This is not a backup and does not qualify the
firmware 1.02 settings layout or enable automatic Terminal Mode entry.
Programming mode temporarily interrupts normal radio operation even though
no settings are written.

On the first firmware 1.02 main-unit USB bench run, entry and both reads
succeeded and exit was acknowledged. USB then disconnected and re-enumerated,
so that earlier implementation's old-handle CAT verification failed. That
capture remains a failed probe. The current command never reuses the old
handle after exit and requires a separate fresh identity proof. A returned USB
device path alone does not prove CAT service or radio identity.

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

An explicit `--port` before `mcp` is required. Only enumerated TM-D750 USB
identities are accepted; no unrelated serial ports are opened or probed and
no endpoint is automatically substituted. Recognition of the panel's USB
identity is not proof that its MCP protocol works. The command cannot run
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
exchange, the tool sends no speculative recovery bytes; follow the printed
instruction to fully power-cycle the radio before reconnecting when MCP exit
is unconfirmed. An acknowledged exit followed by CAT silence is a different
condition and does not itself establish that a power cycle is needed. Do not
kill the process to shorten a pending exchange or close.

An abrupt process termination can leave `report.json` empty, and a filesystem
failure can leave a partial final transcript line. These files are evidence,
not a guarantee of recovery. Captures may contain private radio settings;
review them before sharing. Nothing uploads or deletes them automatically.

### Bounded CAT reacquisition

Both `mcp probe` and `mcp backup` stop original-handle protocol traffic at the
MCP exit ACK; neither CAT nor a baud change follows that ACK. Each workflow
closes and drops that handle before considering a fresh connection. An
incomplete read, missing exit ACK, original close or capture failure, or
cancellation prevents the additional open. The explicit `--port` is required
before `mcp`; the tool never substitutes an automatically selected port.

After the two-second settle wait, one sixty-second readiness dispatch window
covers passive USB enumeration, identity attempts, closes, and waits between
retries. Enumeration polls at 250 ms intervals while the selected endpoint is
absent. Before every fresh open, the exact original path must be enumerated
with unchanged recognized VID/PID. Conflicting metadata, multiple same-role
endpoints, or a different path stop verification; even a macOS callout/dial-in
alias is not substituted for the selected path. No unrelated port receives
CAT traffic.

At most four fresh connections can be opened. Each sends only `ID`, `FV`, and
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
measured firmware timing requirements or a guarantee of recovery. One panel-USB
firmware-1.02 run exercised two silent-ID retries before successful identity
verification, as recorded below. Two later panel backups also completed the
policy, with two and three silent predecessors. These are measured sequences,
not guaranteed recovery bounds. Ordinary guarded setters and the fixed
qualification experiments retain their single-attempt policies and report
shapes. The opt-in managed diagnostic separately reuses bounded identity-only
reacquisition after its guarded exchanges; that complete lifecycle remains
hardware-unqualified. Ordinary CAT and gateway sessions do not acquire
automatic reconnect behavior.

Original-session fields remain separate: a successful read-and-exit phase has
`probe.outcome.status` or `backup.outcome.status` equal to
`"awaiting_cat_verification"`. The required `post_exit_verification` object
records the policy limits, elapsed readiness time, an `attempts` array, one
aggregate `transcript` summary, and the final `outcome`. Each array entry retains
its `enumerations`, optional `connection` with open/identity/close evidence, `outcome`, and
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

The first historical fresh-verification bench run on firmware 1.02 completed both reads and the exit
ACK, then closed the original connection successfully. The serial endpoint
was absent from all forty passive enumeration snapshots, so the workflow
timed out without opening a fresh connection. The port was present at a later
OS-only check. After the operator reported the normal frequency screen, a
separate fresh identity command returned the same model, firmware, and type,
then closed successfully. That observation is separate from the original
failed probe. The exact return time remained unknown; that run's twelve-second
host window was not a qualified recovery bound. A subsequent implementation
extended passive polling from ten to sixty seconds without active retries.
That failed capture is retained unchanged.

Two subsequent main-unit USB runs on firmware 1.02 completed that single-attempt
workflow: both fixed reads, exit ACK, original close, one fresh matching CAT
tuple, and fresh close. USB was first detected at approximately 11.4 and
11.7 seconds after the ACK; CAT identity completed at 11.8 and 12.0 seconds.
The 295 captured bytes matched earlier runs. Both successful samples detected
the endpoint within the old window too, so the increased limit alone does
not explain their success. These bench observations validate the fixed
read/reconnect workflow on this unit, not a universal recovery deadline,
full configuration backup, settings-write compatibility, or the newer retry
policy.

A later firmware 1.02 panel-USB probe completed both fixed reads, acknowledged
exit, and closed the original handle. The endpoint returned, but its sole fresh
`ID` attempt received no bytes before timing out. A later, separate panel CAT
status check succeeded without a host-sent reset or repeated MCP entry. The
original probe remains failed. These observations motivate bounded readiness
handling; they establish neither the exact readiness delay nor the new policy's
live success.

A subsequent, separately approved panel-USB run on September 12, 2026 completed
the fixed reads and acknowledged exit, then exercised the bounded retry path.
The first two fresh connections received no bytes after their ID writes and
closed after the reply timeouts. Following each close, the runner waited two
seconds and re-enumerated the exact endpoint. The third connection returned
matching ID/FV/TY, completing identity 11.72 seconds after the exit ACK, then
closed cleanly. All 295 bytes matched earlier captures; both transcripts were
complete and the process exited successfully. A separate status check reported
both bands FM and Gateway Off. No host reset, repeated MCP entry, settings
write, or RF request was sent. This validates the observed recovery sequence on
that unit, not a universal readiness deadline or other MCP workflows.

`post_exit_verification.outcome.status = "matched"` never rewrites the original
probe outcome. Matching the selected endpoint and CAT tuple is not proof of
physical-unit continuity: paths can be reassigned and this workflow has no USB
serial identity. It does not qualify a settings schema or enable automatic
reopening in ordinary CAT or gateway sessions. Fresh verification is mandatory
for this command; no same-handle or capture-only success path is exposed.

## Diagnostic-only D-STAR probe

By default, `dstar probe` inspects a selected connection without initializing a modem,
connecting a reflector, or requesting mode, routing, configuration, or voice
changes. One panel-USB trial on 2026-09-12, following operator-reported Terminal
selection on a radio previously identified as firmware 1.02, returned MMDVM
protocol 1 with description `TM-D750 RTM1.00`, D-STAR mode, flags zero, and
D-STAR space 127. The connection closed cleanly without modem setup or voice
traffic. This validates that diagnostic exchange once, not initialization,
voice relay, automatic mode changes, or restoration. The modem description
is not the CAT firmware version. Read the help without opening a port:

```bash
cargo run -p tmd750-repl -- dstar probe --help
```

Before a separately approved live trial, establish the actual radio routing
and plan any external mode changes and restoration. Host bytes on other DV/DR
routes can cause automatic RF transmission; diagnostic intent is not an RF
silence guarantee for arbitrary routing. The approval flag acknowledges that
precondition; it does not change or verify routing and does not authorize the
command to enter or leave Terminal Mode.

For that approved trial, select the exact enumerated TM-D750 USB endpoint:

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
Capture files are reserved before opening and existing evidence is never reused.

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
   flush, and synchronize the final report. Cleanup and evidence failures remain
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
Status failure preserves accepted version evidence; Gateway-query failure
preserves CAT identity. Endpoint metadata, duration limits (`secs`/`nanos`),
timestamps, cancellation, signal/close/synchronization failures, and transcript
completeness are explicit. `transcript.jsonl` records exact requested and
completed I/O separately, with timestamps. Required recording blocks further
protocol traffic after capture failure, but connection release is still tried.
Report-publication failure is returned to the caller, not recorded as success.
Malformed UTF-8 or embedded control characters in a version description fail
admission, as do unknown status mode bytes. Valid Unicode descriptions and
the codec's trailing padding remain supported; the raw transcript preserves
the original wire representation, including any reserved flag bits.

Exit zero means complete CAT or complete MMDVM diagnostic evidence, with clean
close and synchronized file contents. Inspect `outcome.kind` to distinguish them;
CAT success is not MMDVM proof. Version/status observations establish neither physical radio
identity nor Reflector Terminal versus Access Point mode. MMDVM has no request
correlation identifier, so a received status also does not prove when the radio
generated it. Neither outcome qualifies automatic Terminal setup or a working
voice relay. Keep the captures private and retain failed attempts unchanged.

### Host-managed diagnostic lifecycle

Add `--manage-terminal` to enter Reflector Terminal for the diagnostic and
restore the captured settings afterward. This path is software-tested; the
complete automatic cycle is not yet hardware-qualified. It does not initialize
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
routing and target page before any write. Changed pages receive durable intent,
one write and immediate complete-page readback. Equal pages are compare-only.
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
Keep failed evidence and do not rerun against stale images.

Success requires the MMDVM diagnostic, clean cleanup, and any owed restoration.
Restoration verification means immediate whole-page readback followed by fresh
CAT identity/Gateway, not another post-exit memory read or power-cycle persistence.
Matching USB paths and CAT tuples does not prove physical-radio continuity.

## Reflector Terminal Mode

On macOS, the one-command Bluetooth workflow is:

```bash
RUST_BACKTRACE=full cargo run -p tmd750-repl -- \
  --trace --timestamps dstar start KQ4NIT REF030C
```

Pair the TM-D750 with the Mac and keep a USB CAT connection available for
independent recovery. With no endpoint options, startup selects exactly one
paired candidate named `TM-D750` or the observed remote name `stm32mp1-ex5240`,
plus one unambiguous USB control endpoint. Names identify candidates, not radio
models; CAT identity and captured configuration still gate the lifecycle.
Identical duplicate address/name inventory records are coalesced; conflicting
records or multiple candidate addresses are refused. If both
USB connectors are present and independently unambiguous, main-unit control is
preferred. It never tries a TH-D75, an unrecognized paired name, or a Bluetooth
serial alias. Explicit selection is available when needed:

```bash
cargo run -p tmd750-repl -- \
  --bluetooth 01:23:45:67:89:AB --control-port /dev/cu.usbmodem101 \
  --trace --timestamps dstar start KQ4NIT REF030C
```

Replace the address and USB path with your radio's. `--control-port` belongs
only to automatic Bluetooth startup; it is not a modem route or a fallback
that receives speculative binary traffic. No new confirmation flags are needed
to request this workflow. Running the command authorizes its documented
Terminal/routing changes and the requested reflector session.
Omit the reflector argument to exercise modem startup and shutdown without
opening a reflector connection.

Startup first verifies independent USB CAT identity and Gateway state. It then
uses one MCP session to capture all standard configuration pages, prepare the
active-PM Bluetooth route and Reflector Terminal selection, compare all complete
guard pages, and write only changed pages with immediate full-page readback.
With Gateway Off, MCP uses the selected Bluetooth connection. When Terminal
is already active, configuration uses the independently verified USB path;
CAT is not injected into a possibly active Bluetooth modem link. Unknown modes,
unsupported subtype/USB function, and identities outside `TM-D750 / 1.02 / K,2,1`
are refused. PM, MY/RPT fields, and unrelated bytes are preserved.

After acknowledged exit, startup retains the Bluetooth owner for the D75-style
two-second settle, then uses a nominal 90-second window of three-second waits,
bounded MMDVM version probes, and same-address/channel reopening. The channel
comes from the successful original opening, not a model constant. Neither MCP
nor its settings transaction is repeated. A complete MMDVM reply must prove the
actual owned connection before modem configuration or reflector traffic begins.
Fatal admission, capture, or cleanup errors stop the loop. Deadlines bound host
admission; joining native work and cleanup may extend wall-clock completion.

Startup creates a private `captures/tmd750-dstar-start-*` directory. Its journal
contains the complete standard configuration snapshot and exact expected and
replacement pages, synchronized before writes. Each JSON record is serialized
in memory before file I/O, avoiding per-token file writes without postponing
its flush or required synchronization. The report retains opening,
transition, cleanup, and restoration failures independently. These files contain
radio configuration and received traffic; treat them as private. Cancellation
remains active through preparation, modem initialization, and reflector setup.
It finishes a started exchange and closes any late successful modem session
before restoring owned settings. An uncertain write, lost owner, or restoration
conflict leaves explicit recovery debt and captured images, never a blind write.

`dstar stop`, EOF, and cleanly retired startup failures restore the exact original
Gateway mode, subtype, and route through fresh USB control, followed by fresh
CAT verification. A session that changed nothing does not claim or undo the
operator's existing Terminal selection. If the modem cannot be safely released,
restoration is withheld and reported.

Two macOS runs on September 13, 2026 completed automatic entry and restoration
on `TM-D750 / 1.02 / K,2,1`, in PM Off, with Gateway initially routed to panel
USB and independent control-panel USB CAT. Each read all 1,138 standard pages,
changed only the Gateway and route bytes, completed E/ACK, reached fresh
Bluetooth MMDVM (`TM-D750 RTM1.00`), and restored the exact original guarded
settings with fresh USB `GW 0` verification. The no-reflector run shut down on
EOF. The default command above authenticated and connected to `REF030C`,
reported incoming reflector voice streams, and restored after `dstar stop`.

These observations cover that configuration, not PM1 through PM5, already-active
Terminal entry, other initial routes or control connectors, cold-start
reliability, or every cancellation and recovery-failure path. Acoustic playback,
operator PTT, bidirectional voice, and power-cycle persistence remain unverified.

The earlier firmware-1.02 Bluetooth bench attempt read all 1,138 standard pages,
then timed out on the first guard reread before any settings write. Startup
refused a speculative MCP exit and never reached modem or reflector setup.
A subsequent independent panel USB CAT check was also silent. That attempt
did not prove MCP exit or restored control access. The later successful cycles
do not reclassify the failure or establish its cause.

### Explicit USB modem startup

An explicit `--port` retains the manual USB modem workflow:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  --trace --timestamps dstar start KQ4NIT REF030C
```

When that selected port answers CAT, startup reads identity and Gateway state,
closes the connection, and makes no setting change. An observed `GW 2` reports
that Terminal is already selected without repeating setup or requesting display
confirmation. That CAT connection is not proved to carry modem traffic. If the
Gateway is routed elsewhere, select that endpoint explicitly with `--port`;
the REPL does not infer routing or pair ports to one physical radio. Unknown
values and query errors do not imply Off or trigger manual setup instructions.

With observed Gateway Off, the diagnostic prints the manual setup below.
Menu 986 guidance follows the selected endpoint's USB identity.
If enumeration cannot identify it, the guidance asks you to match the physical
cable location instead of guessing:

1. Menu 980: `COM+AF In/Out`
2. Menu 986: `USB (Main Unit)` or `USB (Panel)`, matching your connection
3. Menu 651: enter and select the DV Gateway callsign
4. Menu 670: `Reflector TERM Mode`
5. Menus 671 and 672: leave both at `DIRECT`
6. Menu 650: `Terminal Mode` last

DV Gateway uses Band A. The ordinary D-STAR callsign in Menu 610 does not
substitute for the DV Gateway identity in Menu 651.

After the TERM indicator appears, run the explicit-USB command again. CAT silence by
itself proves nothing. Only a completed initial `ID` write followed by a reply
timeout with zero received bytes permits the MMDVM `GET_VERSION` probe. Partial
CAT input, failed writes, or later identity-query timeouts close the connection
without probing another protocol. The probe uses one two-second deadline for
its write and response and requires one complete, decoded version response
before it permits modem
configuration or reflector traffic. A missing, partial, echoed, or non-MMDVM
reply stops the attempt without sending gateway frames. That reply proves
MMDVM framing, but it cannot distinguish Reflector Terminal from Access Point
mode; the Menu 670 and 650 selections above remain operator preconditions.

### Runtime and shutdown

Once MMDVM is proved, either workflow initializes D-STAR, resolves `REF030` from the
local host files described below, and connects module C over DPlus. The runtime
implements forwarding of D-STAR headers, AMBE voice, slow data, and
end-of-transmission markers in both directions. The Bluetooth bench run observed
reflector connection and incoming voice-stream events; it did not establish
acoustic playback or operator-PTT transmission. The manual USB modem workflow
requires separate live validation.
Reflector setup uses a 30-second admission window across host lookup,
authentication, and the handshake. Reflector host-file and DNS lookup runs on
a joined worker without blocking the radio runtime; a slow resolver can extend
return time, but cannot start a late handshake. The shared authentication
client retains its own DNS handling. Modem ownership remains available for
cleanup throughout.
Press Ctrl-C to leave monitoring and reach the `dstar>` prompt; enter
`dstar stop` to disconnect cleanly. Ctrl-C lets any in-flight send finish before
ending both reflector-relay stream directions. At the prompt, connections
remain serviced but reflector relay is paused: voice is not forwarded between
the radio and reflector or saved for later playback. Radio-originated local
echo requests are still handled by the modem gateway. `monitor` resumes live
relay; `status` reports the connection state, and `help` lists commands.
The manual USB workflow leaves Menu 650 persistent. Automatic Bluetooth startup
instead restores changes it owns using the independent USB control path.

Normal shutdown finishes the modem task, recovers the selected transport from
its stream adapter, and closes it. Startup failures follow the same cleanup
path once a modem task exists. Cleanup errors remain visible alongside the
original failure; lost ownership is never reported as a successful close.
Runtime shutdown does not send CAT through the modem connection. Guarded
restoration, when owned, is a separate USB lifecycle after modem release.

The manual states that the interface assigned to DV Gateway does not accept
PC commands while gateway mode is active. The read-only MCP backup's fresh-CAT
reconnect check therefore cannot be reused as the success condition for
enabling Terminal Mode on that same interface. Automatic transition therefore
proves the expected gateway protocol separately; the manual's restriction
also does not prove that another interface remains available for disabling it.
With Gateway routed to panel USB, a firmware-1.02 bench observation did find
main-unit CAT identity and Gateway reads available after manual Terminal entry.
A bounded MCP read and fresh CAT reconnect also succeeded; Gateway still
reported Terminal afterward. This supports the separate guarded exit experiment
above, not an assumption that the voice interface accepts CAT commands.

`dstar start` is a startup-only, long-running command. It cannot be invoked
inside the normal `tmd750>` CAT prompt; quit that prompt first. Omit the
reflector to initialize only the modem. Linking later from `dstar>` is not yet
implemented; stop and restart with a reflector argument. To select a local
module different from the reflector module, use `B:REF030C` (local B, remote C).

## Reflector host files

No local host files are downloaded or installed automatically. Reuse a known-good
`thd75-repl` host list, or obtain the current hostname and UDP port from the
reflector operator and create a UTF-8 text file in the config directory:

| Platform | TM-D750 config directory |
| --- | --- |
| macOS | `~/Library/Application Support/tmd750-repl/` |
| Linux | `$XDG_CONFIG_HOME/tmd750-repl/`, or `~/.config/tmd750-repl/` |
| Windows | `%APPDATA%\tmd750-repl\` |

The same platform root with `thd75-repl` instead of `tmd750-repl` provides the
legacy host files. Both directories are read; neither is modified.

| Filename | Default UDP port |
| --- | --- |
| `DExtra_Hosts.txt` | 30001 |
| `DPlus_Hosts.txt` | 20001 |
| `DCS_Hosts.txt` | 30051 |
| `Local_Hosts.txt` | 30001 |

Each non-comment line is `NAME HOSTNAME UDP_PORT`. The port is optional; an
omitted or unparseable port uses the file's default. Use an explicit numeric
port in local overrides. Blank lines and lines starting with `#` are ignored.
Names omit the module letter: `REF030`, not `REF030C`.

For example, a `DPlus_Hosts.txt` entry has this shape. `reflector.example` is a
placeholder, not a working address; replace it with the operator's current host:

```text
REF030 reflector.example 20001
```

Files are read in the table's order, first from `thd75-repl`, then from
`tmd750-repl`. The last entry for a name wins, so TM-D750 files override legacy
files and `Local_Hosts.txt` overrides the other files in its directory. Missing
files are optional. Other read errors, including invalid UTF-8, fail with the
path and cause; a missing reflector reports the searched locations and syntax.

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
mode [a|b] fm|dv
dv [a|b]
fm [a|b]
normal [a|b]
gateway
terminal
quit
```

Band A is the default when a band is omitted. `dv` selects the ordinary
D-STAR RF operating mode; it does not turn on the separate persistent Terminal
Mode. The `terminal` command explains the manual radio-menu sequence without
writing it.

Startup-only workflows are described above:

- `dstar start CALL [REFLECTOR]` opens a long-running gateway session.
- `dstar probe` captures bounded CAT or MMDVM diagnostics; `--manage-terminal`
  adds guarded entry/restoration through an independent USB control endpoint.
- `mcp probe` and `mcp backup` use dedicated radio connections and private
  capture directories.
- `mcp reentry-probe` runs one approved read-only pair with Gateway Off and
  passive macOS serial-registry observation.
- `mcp text list|show|preview` and `mcp terminal preflight` work offline without
  enumerating or opening a radio endpoint.
- `mcp menu list|describe|show|preview` discovers and inspects registered fields
  offline. `mcp menu apply` uses the ordinary scalar policy, explicit endpoint,
  current backup, and `--apply`; it requires immediate readback and fresh CAT.
- `mcp text set` requires an explicit endpoint, current backup, expected text,
  and `--apply`; it changes PM1's name or PM Off MY1 through dedicated connections.
- `mcp pm1-trial` is the separately approved fixed rename-and-restore experiment.
- `mcp my1-trial` temporarily writes the fixed MY1 callsign and restores it.
- `mcp terminal-exit-trial` is the separately approved guarded Terminal-to-Off
  experiment; it is not invoked by ordinary D-STAR commands.

These workflows cannot run inside the interactive CAT prompt. Quit the prompt
and invoke the desired workflow in a new process; use its `--help` for options.

Supply a CAT command after the options to run it once and exit. The
`dstar start` exception opens a gateway session instead:

```bash
cargo run -p tmd750-repl -- status
cargo run -p tmd750-repl -- dv a
cargo run -p tmd750-repl -- normal a
```

Use `--help` for startup options or `help` for the command list. `help` and
`terminal` can run without opening a radio connection.
