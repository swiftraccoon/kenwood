# thd75-tui

Ratatui dashboard owning a `Radio<T>` from `thd75`.

## Rules

- Make `thd75` compile, test, and lint clean BEFORE touching TUI code; a TUI change built on an incomplete library API has to be redone.
- SM and BY must never be polled directly (the firmware returns spurious Band B spikes). `radio_task` implements the AI-pushed BY-to-SM gate; keep all meter work behind it. The mnemonic rules are in `thd75/AGENTS.md`.
- Direct frequency writes and full CAT memory-record writes are quarantined in the library: `FreqUp`/`FreqDown` step through the verified UP/DW commands and `TuneChannel` recalls memories. Do not present the memory browser as a persisted per-channel editor until every edited field has a write path with verified readback.
- `radio_task`'s reconnect loop rediscovers the device path on purpose: USB re-enumerates with a changed suffix after MCP exit, so `Radio::reconnect` alone is insufficient there.
- On macOS use `BluetoothTransport`, which keeps IOBluetooth and its run loop in an isolated helper process, so the TUI may open it from any thread and contains no CFRunLoop FFI. Only one process may own the RFCOMM channel: close the TUI before running a live-radio example.
- The worker retains transient D-STAR lifecycle ownership and selects Band B separately from the shared modem config. Stop the gateway owner and restore CAT before resuming polling. This panel has no reflector network session.

## Code patterns

- Adding fields to `AprsClientConfig` may need `Box<AprsClientConfig>` in the `RadioCommand::EnterAprs` variant to avoid `clippy::large_enum_variant`.
- Range clamps and variant tables must come from the library's `ALL` consts, `Display` impls, and `MIN`/`MAX` bounds, never from literals (the VoxDelay `.min(30)` bug class). Known literal clamps in `app.rs`: three squelch clamps `.min(6)` should use `SquelchLevel::MAX`, and `next_linked_volume`'s `.min(7)` needs a bound added to `LinkedVolumeLevel` first.
