# thd75 (kenwood-thd75)

## Commands

```bash
THD75_KI4LAX_SPEC=/abs/path.json cargo test -p kenwood-thd75 --test spec_audit -- --ignored
cargo run -p kenwood-thd75 --example bt_native           # read-only native RFCOMM
cargo run -p kenwood-thd75 --example read_validation -- <port>          # read-only
cargo run -p kenwood-thd75 --example if_tap_validation -- <port>        # changes then restores RX settings
cargo run -p kenwood-thd75 --example terminal_mode_validation -- --enter-via-usb <port>  # writes Menu 985/650
cargo run -p kenwood-thd75 --example pf_screen_capture    # writes MCP memory
```

- Sources under `probes/` are not cargo targets; nothing compiles them. Move or register one before expecting a build.
- `if_tap_validation` and `terminal_mode_validation` are the repeatable live hardware checks (CHECK PASS/FAIL lines, nonzero exit). Rerun them after touching the lifecycles they pin.
- Tests calling feature-gated lib APIs need `[[test]] required-features`; a bare test run skips them silently.
- `aprs-is` and `kiss-tnc` are unconditional dev-deps so gated test and example dependency acks still compile under `--no-default-features`; the lib-test ack block in `lib.rs` uses `#[cfg(all(test, not(feature = ...)))]`.
- Doc gates build only default and `--all-features`, so doc links from core modules into gated items are acceptable.

## Layering and ownership

- `types/` and `protocol/` have ZERO async and IO dependencies. Protocol is pure `&[u8] <-> types`, testable without hardware. `transport/` and `radio/` are the async (tokio) layers.
- The macOS OCR bridge is the reason this crate overrides workspace `unsafe_code = "forbid"`. `recognize_text` and its `#[expect(unsafe_code)]` module are `#[cfg(target_os = "macos")]`, so a change to the OCR path can pass a Linux build without ever being compiled.
- `transport::broker::{MainThreadBroker, BrokerHandle}` has no caller since IOBluetooth moved into the helper process. Do not wire new code to it.
- Import shared runtime, probe, and transport types from their own crates; TH-D75 namespaces deliberately do not re-export them.
- `DstarGateway::start` keeps transient entry and exit policy; `start_gateway_mode` requires a connection on which a complete MMDVM exchange already succeeded. Consumers get read-only `modem()` plus thin mutating delegates. Never expose a mutable owned runtime: replacing it detaches its stream from the radio's saved restoration state. Probe success shows MMDVM framing on that connection only: not CAT readiness, radio identity, or the Gateway setting.
- The TH-D75 Bluetooth wrapper owns model policy: default device name `TH-D75`, fixed RFCOMM channel 2, one selected-open retry, a single-attempt probe with no retry, and explicit reopen policy.

## Type and error conventions

- Goal: completely safe for anyone to use, with access to every TH-D75 feature.
- Dependencies stay at the minimum that works, ideally none beyond the shared workspace crates; the rule is in the root `AGENTS.md` section `Standard`.
- Reject invalid values at construction, never at the radio. Public parameters use validated types, never raw u8; out-of-range values from the radio are parse errors, not silent passes.
- All `TryFrom<u8>` impls use `ValidationError`. New out-of-range errors use the generic named-field family (`SettingOutOfRange`, `IntegerOutOfRange`, `InvalidTextByte`, `TextLengthOutOfRange`). Never add positional tuple variants.
- `Error::CommandRejected` and `NotAvailableInCurrentMode` carry the refused CAT mnemonic, attached in `execute()`. Semantic preflight failures get their own variants, never a fake rejection.

## Dual encoding warning

The radio uses DIFFERENT encodings for the same field depending on context:

- CAT wire (FO/ME): FM=0, DV=1, NFM=2, AM=3.
- CAT `MD` and flash memory (MCP, SD card): FM=0, DV=1, AM=2, LSB=3, USB=4, CW=5, NFM=6, DR=7.
- Use `CatChannelRecord` for FO, `CatMemoryChannelRecord` for ME, `StoredChannel` for binary images. Never mix them: same byte, different meaning.

## CAT mnemonic contracts (hardware-verified)

- `TN` is `mode,data-band`: mode 0=off, 1=APRS, 2=KISS, and on exact firmware `1.03.AZM` 3=transient MMDVM; data band 0=A, 1=B. Packet speed is separate (`AS`, Menu 505, KISS Set Hardware). `TN 0,0` in the recovery preamble means TNC off with Band A.
- `DW` is frequency-down, the counterpart of `UP`, not dual watch.
- `BL` is battery level, read-only, observed domain 0-5; values 0-4 are Empty Red, 1/3 Yellow, 2/3 Green, Full Green, Charging, and 5 stays `Unidentified5`. It is not backlight. Charging is reported only on USB power; cradle charging reports plain battery levels.
- `SH` is the mode-indexed IF filter width, not shift.
- `LC` is backlight control (MCP `0x1060`, wire domain 0-3), not a lock. No verified CAT key-lock operation exists.
- `SM` is a read-only per-band nibble meter; no setter exists. `SQ` range is 0-6.
- `AG` is a bare read (`AG\r`, not band-indexed) and a bare three-digit write (`AG 015\r`), firmware domain 000-200.
- Bare `BE` is the APRS beacon transmit action and returns `N` when the TNC is not ready; parameterized `BE` is rejected. It is not a beep preference.
- `FS` is Fine Step (bare read, 0-3); `SF` is Step Size (band-indexed, 0-11). Do not swap them.
- `N` means "not available in current mode", distinct from `?` (unknown command).
- Exact firmware identity `1.03.AZM` selects `FirmwareProfile::AzimuthAutomation`: it repurposes bare `GM` and `GW`, so stock GPS-mode and gateway reads are refused before IO once `FV` identifies it. Automation access exists only through `qualify_automation()`, which attests the exact runtime and ABI 3.

## AI mode (Auto Information)

- `AI 1` makes the radio push BY/FQ/MD/SQ changes unsolicited; `execute()` matches response mnemonics and routes unsolicited frames to the broadcast channel.
- SM and BY must NOT be polled: the firmware returns spurious spikes on Band B. Use AI-pushed BY as the gate (squelch open, poll SM once; squelch closed, report zero).

## Bluetooth SPP

- 9600 baud, 8N1, RTS/CTS, auto-detected by `SerialTransport::open`.
- Menu 983 KISS must match the interface that will carry KISS. Selecting USB frees Bluetooth SPP for simultaneous CAT; selecting Bluetooth requires CAT control to move elsewhere before KISS entry.
- Native macOS Bluetooth runs in an isolated helper process that owns IOBluetooth and its run loop, so callers need no main thread and no CFRunLoop FFI. Closing the transport closes its RFCOMM channel and waits for the channel-closed callback; it does not tear down the shared baseband connection.
- Close the live transport before reconnecting: SPP has one live owner and the helper must finish releasing the channel first. This is ordinary ownership, not persistent macOS Bluetooth state, so do not prescribe daemon restarts or settle delays.
- The `/dev/cu.TH-D75` node exists but does not work; always use `BluetoothTransport`. BT pairing is menu-only (Menu 934) and cannot be triggered over CAT.

## MCP memory and programming protocol

- Settings live at 0x1000-0x10D0, NOT 0x0100. Channel data at 0x4000 (memgroups of 6 channels x 40 bytes + 16 padding = 256 bytes), flags at 0x2000 (4 bytes/entry), names at 0x10000 (16 bytes/entry), 1200 entries.
- Channel byte 0x0A: upper nibble tone mode (bit 7 tone encode, 6 CTCSS, 5 DCS, 4 cross-tone), bit 3 reverse, bit 2 split, bits 1:0 shift direction (0 simplex, 1 +, 2 -). Byte 0x09 upper nibble is mode; 0x0B tone, 0x0C CTCSS, 0x0D DCS. `StoredChannel` in `src/types/channel.rs` is the decoder of record.
- `src/memory/menu_fields.rs` and `data/mcp_d75_menu_schema.json` are generated by `mcp-d75-extract` and carry a `@generated` header; never hand-edit. Regenerate, then rerun the same command with `--check`, which fails on any difference. Exact command: `mcp-d75-extract/AGENTS.md`.
- Entry `0M PROGRAM\r` at 9600 baud only. 57600 crashed a radio; 115200 is untested. Read is `R` + 4-byte address answered by `W` + 4-byte address + 256 data bytes; ACK `0x06` each page; exit is `E`.
- The USB connection drops on programming exit; always reconnect, and wait about 5 seconds first. Page operations also work over native Bluetooth SPP at its fixed 9600 baud.
- `McpError` must NOT reset `McpState` to Idle while a Loaded image exists: a failed write must not wipe the cache. A successful `McpWriteByte` patches the cached `MemoryImage` in place.

## Hardware safety rules

- NEVER send a mnemonic before its handler has been recovered from the raw dispatch table and decompiled by address. When something is unknown, trace it in firmware rather than moving on.
- Commands that act rather than report: `TX` keys the transmitter; `PT 1`/`PT 2`/`PT 3` can transmit once APRS trigger conditions hold; `GM 1` on stock V1.03 reboots into GPS-only mode; `0M PROGRAM` stops normal responses; `UP`/`DW` retune; `MR band,ch` switches the active channel; `BC` changes band; `VM` changes operating mode; `CS` rewrites the APRS MyCallsign (transmitted identity); `BS` selects the antenna connector; `LC` changes backlight; `MS` changes the APRS position source. `SR 0/1/2` only sets scan resume method and is not a reset.
- Direct frequency writes are quarantined: FQ and FO are read-only in the public API and no ME full-record writer is exposed until restore and readback are verified on hardware. Use memory recall, verified UP/DW stepping, or operator input; never hand-assemble raw FO/FQ frames, which can snap to the active step grid or clear unrelated fields.
- `tune_channel()` stays verified through an ME existence read, VM mode switch, and exact MR selector recall. MR requires Memory mode; UP/DW act on the selected VFO context.

## IF tap over CAT

- `IO 0/1/2` is Menu 102 AF/IF/Detect and takes effect only in Single-Band mode on Band B in a non-DV mode. IF mode gives a flat passband about 3-19.5 kHz centered on 12 kHz over the USB audio stream; Detect gives baseband audio. Both route to the speaker and hide Band A, so always restore `IO 0`.
- FO frequency writes and `BC` band changes are rejected with `N` while `IO=IF` is engaged. Retuning therefore runs `IO 0`, then verified UP/DW steps, then `IO 1`. Restore order is output first, saved tuning step before saved frequency, saved output last.
- UP/DW bursts swallow steps: two commands 5 ms apart both echo but the VFO moves once. `retune_if_tap` verifies every step with an FQ readback before sending the next, retries stalls, and fails closed after five consecutive unmoved steps. Never fire-and-forget a step burst.
- The IF tap carries signal ONLY from the VHF/UHF front end; the MW/SW/HF board bypasses the tap point, so HF listening through it is impossible. A squelch-open capture always shows a plausible noise shelf, so only a live carrier at 12 kHz proves the signal path.

## Key gotchas

1. `ME` field 19 is URCALL, not the channel name; display names live in MCP at 0x10000.
2. `ME` has 23 fields, not FO's 21: the extra ME-specific fields are at indices 14 and 22.
3. FO in VFO mode returns values outside normal ranges (shift=8); parsers must tolerate extended values.
4. Wire fields [7]-[12] map to byte[10] bits: tone(7), ctcss(8), dcs(9), cross(10), reverse(11), shift(12); field [12] combines split and direction (0 simplex, 1 +, 2 -, 4 split).
5. `ToneCode` range is 0-49 (50 CTCSS tones), not 35.
6. `SerialTransport::open`/`open_with_baud` check `Handle::try_current` and return a typed `TransportError::Open`; the underlying serial open would still panic without a reactor, so callers on plain threads keep the `rt.enter()` guard.
7. Charge-only USB-C cables charge the radio but never enumerate, so discovery silently falls back to Bluetooth. "Connected via bluetooth" with a USB cable attached is the tell.
8. USB charging desenses 2 m packet RX; never validate RX while charging.

## KISS TNC and APRS glue

- Generic framing lives in `kiss-tnc` (spec-correct per Chepponis/Karn 1987). `KissSession::exit` writes the bare `C0 FF C0` CMD_RETURN frame and returns `DesyncedRadio`, whose `restore` drains the binary residue and re-proves the CAT exchange boundary.
- The reported CMD_RETURN nibble-splitting firmware bug has no handling anywhere in this crate. Re-verify on hardware KISS before writing code that assumes either behavior.
- `AprsClient::next_event` reads `Instant::now()` once per iteration and threads it into every call of the sans-io `aprs` crate, which must never read the clock itself.
- `StoredSmartBeaconingSettings` is the radio-memory binary format; its `TryFrom` bridges to the runtime `aprs::SmartBeaconingConfig` (km/h units).
- Never put real GPS coordinates in committed code or fixtures; use synthetic data.

## Testing

- `MockTransport::expect(cmd_bytes, response_bytes)` backs the unit tests. Expectations are strict FIFO, so wire-order tests are real assertions and `assert_complete()` / `assert_reopen_script_complete()` prove nothing extra was sent or skipped.
- Live Bluetooth work stays in standalone examples rather than `#[test]` so it is an explicit hardware operation. Only one process may own the RFCOMM channel; close the TUI first.
