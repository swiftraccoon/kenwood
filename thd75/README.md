# kenwood-thd75

[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-blue.svg)](https://www.rust-lang.org)
[![License: GPL v2+](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](https://github.com/swiftraccoon/kenwood/blob/main/LICENSE)

Async Rust library for typed control and inspection of the Kenwood TH-D75
amateur-radio transceiver. The public surface includes only operations whose
behavior has been checked against the radio or an exact, lossless file format.

## Features

- **Typed CAT control**: Read identity, frequency, operating state, menus, and
  channel records without exposing wire fields to callers. Validated types
  reject values the radio cannot represent. Memory recall and frequency
  stepping are available; unresolved or lossy writes are absent from the
  public API.
- **MCP programming**: Read the radio's complete programming image, including
  all standard and special channel slots, names, flags, and user settings.
  Page framing, acknowledgements, cleanup, reconnect, and read-back
  verification are handled as one workflow. Factory-calibration pages remain
  read-only.
- **Typed menu settings**: Read and update 400 user-facing menu fields by name.
  Sparse reads fetch only the needed pages. Batch updates preserve unrelated
  bits, enter programming mode once, and verify every changed page.
- **SD card parsing**: Read `.d75` configs, `.nme` GPS logs, callsign and QSO
  tables, `.wav` audio recordings, and `.bmp` screen captures. The repeater
  importer validates Kenwood's exact 31-column catalog, decodes official
  UTF-16LE and Shift-JIS downloads, then applies model/region selection before
  enforcing the radio's 1,500-entry capacity. QSO logs enforce the manual's
  exact 24-column schema and documented wire spellings while preserving every
  other field without reinterpretation.
- **Closed-loop V1.03.AZM automation**: Verify the exact custom firmware before
  accepting input or screen access. Guarded sessions can press front-panel
  keys, capture stable screen frames, verify checksums, and make exact pixel or
  macOS Vision OCR assertions.
- **APRS integration**: High-level `AprsClient` that owns `Radio<T>` + `KissSession` and threads `now: Instant` into the sans-io stack. Packet-radio protocol code (KISS framing, AX.25 codec, APRS parser/digipeater/SmartBeaconing/messaging/station-list, APRS-IS) lives in the sibling [`kiss-tnc`](https://github.com/swiftraccoon/kenwood/tree/main/kiss-tnc), [`ax25-codec`](https://github.com/swiftraccoon/kenwood/tree/main/ax25-codec), [`aprs`](https://github.com/swiftraccoon/kenwood/tree/main/aprs), [`aprs-is`](https://github.com/swiftraccoon/kenwood/tree/main/aprs-is) crates.
- **APRS settings bridge**: Convert the radio's `SmartBeaconing` speed settings
  from the configured display unit into the host-side APRS model.
- **Transport layer**: USB (CDC ACM) and Bluetooth SPP with auto-detection. On
  macOS, potentially unbounded native `IOBluetooth` calls are isolated in a
  killable helper process; Linux and Windows use serial RFCOMM ports.
- **Session resilience**: `Radio::reconnect()` re-establishes a dropped USB or Bluetooth link on the same transport identity (surviving USB re-enumeration and MCP programming-mode exits), and `RadioLinkRecovery` explicitly retries with capped exponential backoff while broadcasting typed link events. MCP writes verify by read-back before reporting success.
- **Async**: Built on tokio. All radio operations are async.

## Cargo features

Both features are enabled by default; a CAT-only consumer can depend with
`default-features = false` for the core control surface alone.

| Feature | Adds | Stays in the core without it |
|---------|------|------------------------------|
| `aprs` | The `AprsClient` stack (radio + KISS session + APRS-IS uplink glue), the `KissSession` binary TNC session, and the `aprs-is`/`kiss-tnc` re-exports | CAT APRS settings, GPS position types, TNC mode commands, and the sans-io `aprs`/`ax25-codec` type layer |
| `dstar` | The `DstarGateway` reflector client and the `MmdvmSession` modem session over the tokio `mmdvm` crate | The Menu 650 terminal-mode lifecycle, MMDVM link diagnosis (`mmdvm-core` only), and CAT D-STAR settings |

## API vocabulary

The public API follows these naming rules:

- `get_*` performs one CAT request/response exchange. `cached_*` performs no
  I/O. `read_*` and `write_*` are multi-exchange or bulk operations that may
  hold the link for their whole workflow.
- `enter_*` sends bytes to put the radio into an exclusive mode and returns a
  session that owns that mode. `into_*` is a local ownership conversion with
  no I/O.
- `Settings` means values resident in the radio. `Config` means host-side
  options or an owned configuration-file model. `Stored*` names values from a
  radio memory image, while `Cat*` names CAT text records.
- A `new` constructor returns `Result<_, ValidationError>` whenever its input
  can be outside the represented domain. Unit-bearing constructors and
  accessors state the unit, such as `from_khz` and `as_milliseconds`; raw
  representation access uses `as_raw`.
- `Default` is reserved for an empty, disabled, or representation-neutral
  value. A documented radio factory state is named `factory_default`; if that
  state depends on region, band, or display units, the required context is an
  argument instead of an implicit global default.
- Rust identifiers spell the acronym `Dstar`; operator-facing prose uses
  `D-STAR`.

## Quick start

```rust,no_run
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Auto-detect USB port
    let ports = SerialTransport::discover_usb()?;
    let port = ports.first().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no TH-D75 USB port found")
    })?;
    let transport = SerialTransport::open(&port.port_name)?;

    let mut radio = Radio::new(transport);

    let version = radio.get_firmware_version().await?;
    println!("firmware {version}");

    let freq = radio.get_frequency(kenwood_thd75::types::Band::A).await?;
    println!("Band A: {freq}");

    Ok(())
}
```

## Tests

Optional comparisons in `tests/spec_audit.rs` require an untracked,
third-party JSON fixture. They are ignored by default, so `cargo test` reports
the skipped coverage instead of counting unexecuted comparisons as passes.
Fixture-independent checks in that target still run normally.

Run only the external-spec comparisons with an explicit fixture:

```text
THD75_KI4LAX_SPEC=/absolute/path/to/ki4lax_cat_spec.json \
  cargo test -p kenwood-thd75 --test spec_audit -- --ignored
```

This explicit run fails if the variable is unset, the fixture cannot be read,
or its JSON/schema is malformed.

## Examples

Runnable examples live in [`examples/`](https://github.com/swiftraccoon/kenwood/tree/main/thd75/examples). Run any of them with `cargo run -p kenwood-thd75 --example <name>`:

| Example | Description |
|---------|-------------|
| `identify` | Print the typed radio model ID, firmware identity, serial number, model code, region, hardware variant, and power status. |
| `monitor` | Poll S-meter, frequency, mode, and busy state on both bands every 250 ms. |
| `tune` | Recall a populated memory channel, switching the selected band into memory mode when needed. |
| `channel_dump` | Read memory channels 0-999 via CAT, optionally reading display names via MCP. |
| `config_backup` | Read the entire 500 KB radio memory via MCP and save it to a binary file. |
| `write_settings` | Temporarily change and restore squelch via CAT, then overwrite channel 0's display name via MCP. |
| `mcp_menu` | List, read, validate, and batch-write generated MCP-D75 menu fields using sparse page access. Writes are dry-run by default. |
| `read_validation` | Trace and compare live, read-only FV/AE/TY/FQ/FO/MR/ME/RT responses against their lossless typed results. |
| `if_tap` | Capture AF, 12 kHz IF, and detector audio from the current Band B frequency, then restore the original radio settings. The operator must tune Band B first. |
| `verify_state` | Attest and inspect supported modified-firmware memory-read targets for qualification and offset discovery. |
| `bluetooth` | Connect over native macOS Bluetooth or a Linux/Windows serial RFCOMM port (pair via Menu 934 first). |
| `bt_native` | Exercise the native `IOBluetooth` RFCOMM transport (macOS). |
| `pf_screen_capture` | Assign the front-panel PF1 key to Screen Capture via an MCP memory write. |
| `kiss_monitor` | Decode KISS frames, AX.25 packets, and APRS position reports from the TNC. |
| `automation_probe` | Qualify V1.03.AZM, retain authenticated screen BMPs, exercise MENU/navigation, restore the UI, and require exact OCR text (macOS only). |
| `automation_tap` | Execute one exact guarded key tap and retain before/after pixel and optional OCR evidence (macOS only). |
| `automation_audit` | Audit all 217 reviewed menu leaves, or an explicitly scoped subset, using guarded-input and screen evidence (macOS only). |
| `hardware_audit` | Run a fixed, read-only CAT capability audit; the automation profile exact-attests CAT identity `1.03.AZM` and excludes its `GM`/`GW` command collisions. |

## Batch MCP menu writes

List fields or filter them by name:

```text
cargo run -p kenwood-thd75 --example mcp_menu -- --list beep
```

Take a structurally read-only sparse snapshot, optionally filtered by field
name:

```text
cargo run -p kenwood-thd75 --example mcp_menu -- \
  --read interface --port /dev/cu.usbmodem1234
```

Snapshot values can contain private callsigns, saved coordinates, messages,
and Bluetooth device names. Keep the output local or send it only to a trusted
destination. Catchable termination signals trigger MCP recovery; an
uncatchable process kill or host power loss can still require power-cycling
the radio.

Build and validate a patch plan without touching the radio:

```text
cargo run -p kenwood-thd75 --example mcp_menu -- \
  radio.Beep=on radio.BluetoothOnOff=off
```

Apply multiple assignments in one MCP programming session:

```text
cargo run -p kenwood-thd75 --example mcp_menu -- \
  --write --port /dev/cu.usbmodem1234 \
  radio.Beep=on radio.BluetoothOnOff=off
```

Enum values accept the official English label, decompiled member name, decimal
raw value, or `0x` value. Numbers resolve as `0x` hex first, then as the
decimal raw value whenever the field accepts that raw, then as an option
label, so a numeric label can never capture a valid decimal raw. Fixed
strings accept text. Raw bitmap fields accept `hex:...` or `@FILE`. The
command reads only pages referenced by the patch, writes only pages that
actually change, and verifies each write by read-back.

## V1.03.AZM closed-loop automation

`Radio::qualify_automation()` is available only for the exact hash-pinned
V1.03.AZM automation firmware; it is not a generic stock-firmware interface.
Qualification verifies the exact model and firmware identity, every patched
hook, the complete 1,300-byte runtime, the ABI 3 reply, crossing-read refusal,
and stable metadata before returning an exclusive `AutomationSession`.

The session can issue a complete 40 ms key tap and capture one stable 240×180
RGB565 frame. Capture acceptance requires an advancing generation, identical
even-seqlock metadata around the transfer, exact geometry/format fields, and a
matching host-computed IEEE CRC-32. The compressed path must decode to exactly
86,400 bytes; malformed RLE poisons the session, while an explicit firmware
overflow selects the raw aperture.

On macOS, `ScreenFrame::recognize_text()` combines native-resolution and
deterministically enlarged Vision passes. `require_unique_text()` then applies
case-sensitive exact text, confidence, and normalized-screen ROI conditions.
A successful key reply alone is never a UI assertion.

Run the retained MENU round trip:

```text
cargo run -p kenwood-thd75 --release --example automation_probe -- \
  --exercise-menu --expect-baseline-text 146.940 --expect-menu-text Menu
```

Probe one selected-menu transition:

```text
cargo run -p kenwood-thd75 --release --example automation_probe -- \
  --menu-navigation-key 05 --expect-navigation-text APRS
```

The separate `automation_tap` recovery helper requires exact qualification;
use it to execute one key when deliberately recovering an already-known UI state:

```text
cargo run -p kenwood-thd75 --release --example automation_tap -- \
  01 146.940 /private/tmp/thd75-menu-recovery
```

### Guarded input and menu audit

The exact ABI query is also the guarded-input session boundary: it seqlock-
invalidates any snapshot retained from an earlier qualified session before it
replies. Requalification therefore never inherits a stale input lease, and a
new `capture_screen()` must precede every guarded operation after qualification.

After `capture_screen()`, V1.03.AZM gives that session one short-lived guarded-input
lease. `guarded_tap_key()` or `guarded_tap_keys()` consumes the lease exactly
once. The latter accepts one to three keys. Before each press, the firmware
byte-compares the live 86,400-byte framebuffer with the frozen raw snapshot. A
match dispatches the press and the host always attempts its
release; a mismatch returns an authenticated `ContextChanged` outcome without
pressing or releasing that key. A 1.5-second dispatch-exchange deadline starts
as the first guarded press begins; post-ACK metadata validation is outside that
bound. After transfer, metadata, and CRC validation, the host may hold the
lease for at most five seconds while performing semantic checks before I/O;
raw frame-transfer time is excluded because firmware's live comparison is the
actual context guard. Exact command-count and seqlock continuity authenticate
every receipt and the final outcome. Cancelling after
a successful press can prevent its release, poisons the session, and requires
reconnect plus explicit UI recovery.

`guarded_decimal_route()` sends all three decimal digits in one
`GM RDDD,SS` exchange. Firmware performs one full-frame guard before any input,
then emits all three press/release pairs synchronously with zero hold. One
command-4 metadata receipt authenticates the packed route, guard count `1`,
either completed taps/event mask `3`/`0x3F` or `0`/`0x00`, status,
command-count +1, and seqlock +2. This removes all host transport, OCR, and
filesystem gaps between digits. It does not remove the residual race in which
a concurrent writer changes an already-compared framebuffer word. An
authenticated refusal is always before the first digit and leaves the session
usable; ABI 3 rejects every partial-refusal receipt. Recovery never assumes an
undocumented numeric-entry timeout.
Zero-hold behavior is not treated as hardware-qualified until the live runner
opens harmless information page 991, proves the exact Version / V1.03.AZM
screen, and restores the exact baseline frame.

After independently validating the snapshot's screen semantics, the core host
sequence is:

```rust,no_run
use kenwood_thd75::radio::automation::{
    GuardedDecimalRoute, GuardedDecimalRouteOutcome,
};

# async fn guarded_route_example<T: kenwood_thd75::Transport>(
#     mut radio: kenwood_thd75::Radio<T>,
# ) -> Result<(), Box<dyn std::error::Error>> {
let mut session = radio.qualify_automation().await?;
let menu = session.capture_screen().await?;
// Establish the expected screen with an application-specific pixel/OCR check
// before using `menu` as the guarded-input lease.
let outcome = session
    .guarded_decimal_route(&menu, GuardedDecimalRoute::new([9, 2, 0])?)
    .await?;

if !matches!(outcome, GuardedDecimalRouteOutcome::Dispatched(_)) {
    return Err("guarded menu route was not fully dispatched".into());
}
# Ok(())
# }
```

`automation_audit` uses that one-command path for complete decimal menu routes;
it never sends overloaded numeric keys through the ordinary tap path. The
runner derives and validates this exact partition from the reviewed V1.03
manual: 162 typed value or information pages are entered and validated, 14
row-only safe-inspection pages are entered under page-specific read-only screen
oracles, and 41 leaves are located but never entered (16 destructive or
external actions and 25 multi-record or editor pages). Together those classes
cover all 217 menu rows
without overlap. Startup canaries and home-profile checks use three captures;
ordinary menu checks use one firmware-authenticated stable capture after the
settle delay. Exact menu titles, numbered-row or singleton-submenu locators,
typed values, BMP/OCR/metadata evidence, and restoration to the reviewed
dual-band home profile are recorded in JSONL. Every capture owned by one of
four explicitly recognized high-risk
menu audits reduces body and selected OCR text to SHA-256: 516 (APRS Object),
651 (My Callsign), 935 (Bluetooth Device Information), and 946 (Secret Access
Code). Redaction follows the known menu-audit context, so missing or duplicate
title OCR cannot disable it. Incidental callsigns, messages, and device data
can remain elsewhere, so treat all JSONL, BMP, and binary-snapshot artifacts as
private.

The home oracle compares every RGB565 pixel except full-width row ranges
`y=[0,20)`, `y=[31,45)`, and `y=[131,145)`, plus the live RF S-meter rectangle
`x=[0,151), y=[90,101)` on the 240x180 framebuffer. The S-meter exclusion is
only that exact 151x11 rectangle, not its full-width rows. Ordered frequency
and mode text anchors remain mandatory.

An entered current-value page receives `[MODE]` as the next and only ordinary
key, followed by an exact numbered-row proof. A safe-inspection page is even
more restrictive: OCR must first prove the literal `Back` in the bottom-left
soft-key region, with every matching high-confidence observation overlapping
at one rendered locus. Only then may the runner interpret `[MODE]` as Back and
dispatch it once. A missing Back locus or matching candidates at distinct loci
fails closed without a key press. Character-editor leaves 564 (Reply Message
Text), 583 (`UIdigi Aliases`), 594 (Message Group Code), 595 (Bulletin Group
Code), 652 (RPT1), 653 (RPT2), 654 (Device Information), 903 (Power-on Message),
and 946 (Secret Access Code) are therefore never entered. Menu 654 is
explicitly locate-only, not a safe inspection: its page exposes `Space` and
`Clear` editor controls. Menu 946 is likewise locate-only because its Secret
Access Code page is a character editor, not a read-only inspection surface.
These editor-specific soft-key surfaces can put an operation such as `Space`,
rather than `Back`, at bottom left; locating their exact numbered rows does not
authenticate a safe exit from the editor.

Menu 710 is the stock V1.03 singleton-submenu exception. `FM Radio List` is the
only reviewed leaf beneath the exact `FM Broadcasting` / `71-` / selected
`Memory` page. Activating that selection can enter the multi-record list, so
the runner treats the exact title, prefix, 24-pixel selection band, Back/OK
controls, and one-leaf manifest relationship as the terminal non-entry locator.
It does not send the activation key.

Before and after the screen audit, the runner byte-compares the 350 complete
MCP pages spanned by all 400 generated menu-field descriptors. That final-state
equality does not prove that no intermediate write occurred, and explicitly
excludes the other 1,605 MCP pages and non-MCP transient or volatile state.

Menu 134 has one stock V1.03 prerequisite: its Priority Scan page refuses to
open when the Pri special-memory record is empty. The runner requires Priority
Scan to be Off, fsyncs owner-private copies of complete MCP pages `0x0031` and
`0x00F7`, and leaves an existing valid Pri record unchanged. If Pri is empty,
it temporarily copies only stock WX1's retained flag byte and exact 40-byte
162.550 MHz FM simplex record. Compare-and-exchange writes the data page before
the validity flag; cleanup invalidates Pri first, restores the data page, and
then rereads both full pages for byte-exact equality. Those two pages are
verified independently because they are outside the 350-page menu-field
snapshot. The durable `menu-134-pri-pages-before.bin` artifact remains the
recovery source if power or the process is lost between the ordered writes.

The runner's first CAT/MCP operation is exact `1.03.AZM` qualification. It then runs the
missing-snapshot, changed-context, and live zero-hold 991 canaries before any
MCP access. After the before-audit MCP snapshot it requalifies V1.03.AZM and requires
the exact initial home framebuffer before auditing leaves.

Those menu numbers, labels, option order, setting semantics, UI resources, and
generated MCP offsets remain applicable to stock V1.03. The custom-overlay-only
parts are the `GM`/`GW` transport, framebuffer/key guard, absolute runtime
addresses, and Menu 980 USB-storage apply behavior.

Run the full audit into a new owner-private absolute directory:

```text
cargo run -p kenwood-thd75 --release --example automation_audit -- \
  --port /dev/cu.usbmodem1234 --output-dir /private/path/new-audit-directory
```

Use `--device TH-D75` instead of `--port PATH` for native Bluetooth. The two
endpoint options are mutually exclusive; omitting both preserves the existing
`TH-D75` Bluetooth default.

Use `--menu 991` for one leaf, or `--start NUMBER --limit COUNT` for a bounded
slice. Only complete 217-leaf coverage can report `FULL_PASS`; explicit subsets
report `SCOPED_PASS`. The runner requires macOS Vision; radio transport may be
an explicit USB CDC path or native Bluetooth. It writes its private evidence
bundle only beneath the requested owner-private output directory.

The V1.03.AZM firmware package has been physically flashed: CAT `FV` returns
the exact stored identity `1.03.AZM`, ABI 3 qualification succeeds, and the
typed read-only CAT validation passes on hardware. The unchanged ABI 3 runtime
and hooks also passed a complete
TH-D75A/V1.03 hardware run before the final displayed firmware identity was
applied: `FULL_217_ROWS_162_VALUES_14_SAFE_INSPECTIONS_PASS`. All 217 rows were
attempted, located, and restored; 162 value/information pages and 14 safe
inspections were validated, 41 editor/action pages were not entered, and the
run reported zero inconclusive results or errors. The before/after MCP
snapshots each contained 350 pages (89,600 payload bytes; 90,300-byte raw
artifact) spanning 400 schema fields. Both had SHA-256
`d08367d70813f5edb822757cad700416e770b59621741cae632573864439734e`,
and the raw artifacts compared byte-for-byte. Final screen proof restored Band
A to 146.940 MHz and the quiet Band B baseline to 446.000 MHz, with operation
band B. The retained JSONL, BMP, and snapshot bundle is private and is not part
of the repository.

## Supported connections

| Platform | USB | Bluetooth |
|----------|-----|-----------|
| macOS | `/dev/cu.usbmodem*` | Native `IOBluetooth` RFCOMM |
| Linux | `/dev/ttyACM*` | `/dev/rfcomm*` via `SerialTransport` |
| Windows | `COM*` | BT COM port via `SerialTransport` |

On macOS, `BluetoothTransport` runs a signed executable containing its private
RFCOMM helper constructor because Apple's synchronous and asynchronous write
paths can both block indefinitely when flow-control credit stalls. Command-line
clients use their current executable. Sandboxed applications embed a helper
signed for sandbox inheritance and pass its absolute path to
`BluetoothTransport::open_with_helper_executable`. Radio selection accepts an
exact paired-device name or Bluetooth address, with the address taking
precedence. A name shared by multiple paired devices is rejected so callers can
select the intended radio by its exact address.

The parent communicates through nonblocking raw pipes. A failed, timed-out, or
cancelled write destroys the helper because the transmitted prefix is
ambiguous; close, reconnect, and drop do the same. A command-response read
timeout instead leaves the byte stream available for the radio layer's
stale-response drain, while helper EOF or a read error destroys it. Cleanup is
bounded and cannot wedge the application. A dedicated liveness-pipe watchdog
also exits an orphaned helper if its parent disappears. Native startup uses one
20-second SDP/baseband/RFCOMM deadline beneath the parent's 22-second bound, and
buffers radio ingress until the readiness prefix is complete. One helper owns
the TH-D75 SPP channel per process.

A newly launched helper can observe an already-connected Bluetooth baseband
before that process's Classic manager is ready to open RFCOMM. If the first
bounded helper reports `NotFound`, `BluetoothTransport::open()` waits one second
and retries exactly once in a new helper; all other errors return immediately,
and `reopen()` inherits the same two-attempt maximum. The one retry addresses a
process-local readiness/baseband race in the fresh helper. Recovery leaves the
shared baseband and macOS system Bluetooth services alone.

## Radio compatibility

The CAT and MCP schema is aligned with stock Kenwood V1.03. Live typed reads and
closed-loop automation are validated on a TH-D75A running exact `1.03.AZM`
(automation ABI 3). The TH-D75E (European model) has different TX frequency
ranges but uses the same protocol.

## License

GPL-2.0-or-later
