# TM-D750 REPL

`tmd750-repl` is a plain-text USB shell for the Kenwood TM-D750. It is designed
to work well with a screen reader. It supports normal CAT control plus an
experimental, protocol-gated D-STAR Reflector Terminal Mode path.

The current write surface is deliberately small. The REPL can select FM or
D-STAR DV on either band, and every selection requires an immediate readback
from the radio. It can read the persistent DV Gateway state, but it does not
write DV Gateway or Terminal Mode settings. It does not expose arbitrary CAT
commands or arbitrary MCP memory writes. Automatic Terminal Mode entry remains
disabled, and general MCP settings writes are refused on firmware 1.02.
The narrow exception is PM1's name: an explicit text setter and a separate
fixed rename-and-restore qualification experiment are described below.
The manifest's
declared firmware label 1.00 is a project compatibility gate, not an extracted
vendor maximum-version restriction.

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

The main-unit CAT connection has been hardware-validated. Recognizing the
control-panel USB identity does not establish equivalent live protocol support.
The transport does not automatically reopen after a USB disconnect: serial
pathnames can be reassigned. Reconnect the intended radio and select its current
endpoint for a new session.

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

This complete read/exit/reconnect sequence was bench-validated on main-unit USB
with firmware 1.02 on September 7, 2026. Every captured page matched its raw
wire response, both connections closed cleanly, and fresh CAT identity matched.
This observation qualifies the read workflow on that unit, not settings writes.

The command closes and drops the original handle after the exit ACK, then uses
the same bounded, single-attempt fresh CAT verification described below. An
explicit `--port` is mandatory. Cancellation finishes the current exchange;
incomplete framing prohibits speculative exit or recovery commands.

`report.json` uses format version 3 with `operation = "configuration_backup"`.
Its `backup.segments` retain every fully acknowledged page even if a later page,
cleanup, or fresh verification fails. Addresses and lengths are explicit; unread
gaps are absent. Original and fresh transcripts remain separate. A successful
exit requires the entire page schedule, both clean closes and captures, matching
fresh identity, and a written, flushed, synchronized report.

This is a standard-region backup, not a full memory dump or a restorable `.d750`
file. The official application seeds omitted bytes from its current model;
knowing the file header does not justify filling those gaps with guessed values.
A compatible export still needs a validated matching template. Keep captures
private: they can contain callsigns, messages, and other personal settings.

### Set the PM1 name

`mcp text set` changes only the global PM1 label. Start with a successful
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
1.02 / type K,2,1 identity are accepted. Other text settings, per-slot selectors,
arbitrary addresses, and firmware overrides are refused. Equal expected and
replacement names are rejected before radio access, not reported as a live
confirmation of the current setting.

Before writing, the entire freshly read PM1 page must match the backup, not
just its name. The command synchronizes a private journal containing both
complete pages, sends one page write, and compares its immediate readback.
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
exit is unconfirmed, fully power-cycle before reconnecting. A power cycle does
not establish which name is stored. This workflow requires Unix private-file and
directory synchronization support.

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
an update. Only the separate PM1 setter above offers a live text write; there
is no generic keyboard injection or automatic Terminal Mode settings write.

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

## Fixed MCP probe

The startup-only `mcp probe` command captures a small, fixed protocol check. It
reads CAT identity, enters MCP programming mode, reads 40 bytes at address 8
and 255 bytes at address 327681, then verifies MCP exit and unchanged CAT
identity. It sends no settings writes, fill commands, arbitrary memory requests,
or RF transmission requests. This is not a backup and does not qualify the
firmware 1.02 settings layout or enable automatic Terminal Mode entry.
Programming mode temporarily interrupts normal radio operation even though
no settings are written.

On the first firmware 1.02 main-unit USB bench run, entry and both reads
succeeded and exit was acknowledged. USB then disconnected and re-enumerated,
so CAT verification on the old connection failed. The command reports this
as a failed probe and does not reopen automatically. A returned USB device
path alone does not prove that CAT service or the same radio identity returned.

Leave the radio on its normal screen and close other radio applications first:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp probe --output bench-01
```

The explicit output directory must not exist; its parent must already exist.
Omit `--output` to create a unique session directory below `./captures/`.
Existing captures are never overwritten. Both output files are reserved before
the serial connection opens. On Unix the directory is created with permissions
0700 and the files with 0600. Windows uses inherited filesystem permissions.

Only enumerated TM-D750 USB identities are accepted, including when `--port`
is explicit. No unrelated serial ports are opened or probed. Auto-discovery
refuses to choose between multiple endpoints. Recognition of the panel's USB
identity is not proof that its MCP protocol works. The command cannot run
inside the CAT prompt; quit that prompt and start a dedicated process.

Each capture contains:

- `report.json`: format version 1, software version, UTC start and finish,
  selected USB path and VID/PID, CAT baud, pre-entry and post-exit identities,
  accepted entry reply, acknowledged fragments, exit disposition, outcome,
  and any connection, signal, capture, or close failures.
- `transcript.jsonl`: sequential transport events with exact UTC Unix
  nanoseconds, monotonic elapsed microseconds, raw byte arrays, baud changes,
  and transport errors. A write request is recorded before dispatch; a separate
  completion or failure records the transport result. A failed write does not
  prove how many bytes reached the radio. Read events contain only returned
  bytes, including empty arrays for zero-length reads. Each event is flushed
  independently of optional trace logging.

Report fragments have explicit addresses and lengths; gaps are not fabricated
into a full memory image. Uniform-fill responses are expanded in the report,
while the transcript retains the actual response bytes. Error objects preserve
the message and underlying cause chain. A successful protocol check is
`probe.outcome.status = "complete"`; exit and transcript completeness are
reported separately. The process exits nonzero on cancellation or any protocol,
capture, signal, or connection-close failure.

Ctrl-C requests cancellation after the current complete exchange. If the
connection remains synchronized, MCP exit and CAT verification still finish
before the connection closes. A transcript write failure requests the same
boundary-safe stop without interrupting cleanup. After an incomplete protocol
exchange, the tool sends no speculative recovery bytes; follow the printed
instruction to fully power-cycle the radio before reconnecting. Do not kill
the process to shorten this wait.

An abrupt process termination can leave `report.json` empty, and a filesystem
failure can leave a partial final transcript line. These files are evidence,
not a guarantee of recovery. Captures may contain private radio settings;
review them before sharing. Nothing uploads or deletes them automatically.

### Optional fresh-connection verification

To test return to CAT through a fresh USB handle, explicitly select the port
and opt in:

```bash
cargo run -p tmd750-repl -- --port /dev/cu.usbmodem101 \
  mcp probe --verify-reconnect --output bench-02
```

This variant reads the same two fragments and awaits the MCP exit ACK, but
does not send post-exit CAT on the original handle. It closes and drops that
handle before considering a fresh connection. An incomplete fragment, missing
exit ACK, original close or capture failure, or cancellation prevents the
additional open. The explicit `--port` is required before `mcp`; the tool
never substitutes an automatically selected port.

The host waits two seconds to settle, then allows up to sixty seconds of passive
USB enumeration, polling at 250 ms intervals while the selected endpoint is
absent. These are bounded host policies, not measured firmware timing
requirements. The exact original path must return with unchanged recognized
VID/PID. Conflicting metadata, multiple same-role endpoints, or a different
path stop verification; even a macOS callout/dial-in alias is not substituted
for the selected path. No unrelated port receives CAT traffic.

At most one fresh connection is opened. It sends only `ID`, `FV`, and `TY`,
compares the complete identity tuple with the original, and closes. Open or
identity errors and identity mismatches are never retried. Connection closes
have a two-second bound; the passive enumeration budget does not cancel an
in-flight CAT exchange. Ctrl-C finishes the current exchange or identity
attempt and connection close, then skips subsequent work.

Opt-in captures use report format version 2. Existing original-probe fields
remain separate: a successful read-and-exit phase has
`probe.outcome.status = "awaiting_cat_verification"` and no original-handle
`cat_identity`. The additional `post_exit_verification` object records passive
enumeration snapshots, the single open/identity/close attempt, its outcome,
and its independent transcript summary. `post-exit-transcript.jsonl` is
reserved before the initial radio open and records timestamped waits,
enumerations, open requests/results, and actual fresh-connection transport
events. Snapshots include recognized radio endpoints and metadata needed to
explain conflicts, not unrelated serial or Bluetooth inventory.

Enumeration times in the report are measured from the start of passive
polling, after the settle wait. When both absence and presence are observed,
the timestamped transcripts bracket those observations relative to the exit
ACK and record when the fresh CAT responses complete. These are host observations with
polling and scheduling uncertainty, not exact firmware-ready timestamps.

Exit status is zero only when the original fragments, exit ACK, original
close/capture, matching fresh CAT tuple, and fresh close/capture all succeed.

The first opt-in bench run on firmware 1.02 completed both reads and the exit
ACK, then closed the original connection successfully. The serial endpoint
was absent from all forty passive enumeration snapshots, so the workflow
timed out without opening a fresh connection. The port was present at a later
OS-only check. After the operator reported the normal frequency screen, a
separate fresh identity command returned the same model, firmware, and type,
then closed successfully. That observation is separate from the original
failed probe. The exact return time remained unknown; that run's twelve-second
host window was not a qualified recovery bound. The passive budget has since
been extended from ten to sixty seconds, without adding active retries.
That failed capture is retained unchanged.

Two subsequent main-unit USB runs on firmware 1.02 completed the opt-in
workflow: both fixed reads, exit ACK, original close, one fresh matching CAT
tuple, and fresh close. USB was first detected at approximately 11.4 and
11.7 seconds after the ACK; CAT identity completed at 11.8 and 12.0 seconds.
The 295 captured bytes matched earlier runs. Both successful samples detected
the endpoint within the old window too, so the increased limit alone does
not explain their success. These bench observations validate the fixed
read/reconnect workflow on this unit, not a universal recovery deadline,
full configuration backup, or settings-write compatibility.

`post_exit_verification.outcome.status = "matched"` never rewrites the original
probe outcome. Matching the selected endpoint and CAT tuple is not proof of
physical-unit continuity: paths can be reassigned and this workflow has no USB
serial identity. It does not qualify a settings schema or enable automatic
reopening in ordinary CAT or gateway sessions. Without `--verify-reconnect`,
the existing single-connection workflow and version 1 capture remain in use.

## Reflector Terminal Mode

The startup command matches the established TH-D75 workflow:

```bash
RUST_BACKTRACE=full cargo run -p tmd750-repl -- \
  --trace --timestamps dstar start KQ4NIT REF030C
```

The first run while the radio still answers CAT is diagnostic. It reads the
radio identity and DV Gateway state, makes no setting change, and prints the
manual setup. Menu 986 guidance follows the selected endpoint's USB identity.
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

After the TERM indicator appears, run the same command again. CAT silence by
itself proves nothing. The REPL sends only an MMDVM `GET_VERSION` probe and
requires one complete, decoded version response before it permits modem
configuration or reflector traffic. A missing, partial, echoed, or non-MMDVM
reply stops the attempt without sending gateway frames. That reply proves
MMDVM framing, but it cannot distinguish Reflector Terminal from Access Point
mode; the Menu 670 and 650 selections above remain operator preconditions.

Once MMDVM is proved, the REPL initializes D-STAR, resolves `REF030` from the
local host files described below, connects module C over DPlus, and relays
D-STAR headers, AMBE voice,
slow data, and end-of-transmission markers in both directions. This path still
requires live TM-D750 qualification before it should be described as supported.
Press Ctrl-C to leave monitoring and reach the `dstar>` prompt; enter
`dstar stop` to disconnect cleanly. Ctrl-C lets any in-flight send finish before
ending both reflector-relay stream directions. At the prompt, connections
remain serviced but reflector relay is paused: voice is not forwarded between
the radio and reflector or saved for later playback. Radio-originated local
echo requests are still handled by the modem gateway. `monitor` resumes live
relay; `status` reports the connection state, and `help` lists commands.
Menu 650 remains persistent and must be set to Off on the radio before this
USB port returns to CAT.

The manual states that the interface assigned to DV Gateway does not accept
PC commands while gateway mode is active. The read-only MCP backup's fresh-CAT
reconnect check therefore cannot be reused as the success condition for
enabling Terminal Mode on that same interface. A future automatic transition
must qualify the expected gateway protocol separately; the manual's restriction
also does not prove that another interface remains available for disabling it.

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
`RUST_LOG=kenwood_tmd750=trace`. It does not enable a file log. Logs can contain
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
- `mcp probe` and `mcp backup` use dedicated radio connections and private
  capture directories.
- `mcp text list|show|preview` and `mcp terminal preflight` work offline without
  enumerating or opening a radio endpoint.
- `mcp text set` requires an explicit endpoint, current backup, expected name,
  and `--apply`; it changes only PM1's name through dedicated connections.
- `mcp pm1-trial` is the separately approved fixed rename-and-restore experiment.

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
