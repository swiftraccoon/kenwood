# mmdvm

Tokio async shell for the MMDVM digital-voice modem protocol. Wraps [`mmdvm-core`](../mmdvm-core/) for the event-loop side.

## Scope

- `AsyncModem::spawn` — spawns a background task that owns a `Transport`, drives `mmdvm-core` frame I/O, and returns an `AsyncModemHandle` for commands and events.
- `transport` module — `Transport` trait with serial implementations (USB CDC for the TH-D75 internal modem; generic serial for Pi-Star / ZUMspot / MMDVM_HS_Hat hardware).
- Periodic 250 ms `GetStatus` polling that corrects local buffer-space estimates from modem reports. Matches the reference MMDVMHost main loop.
- Per-mode TX queues drained only when the modem reports FIFO-slot availability. D-STAR queue wired today; DMR / YSF / P25 / NXDN / POCSAG / FM present in core but not driven from this shell yet.

## Scope boundaries

Wire-format codec lives in [`mmdvm-core`](../mmdvm-core/). This crate is I/O and scheduling only.

## Reference

Mirrors the C++ `MMDVMHost` main loop at `ref/MMDVMHost/` (GPL-2.0-or-later). Portions are derived from MMDVMHost by Jonathan Naylor G4KLX, 2015–2026.

- MMDVMHost: <https://github.com/g4klx/MMDVMHost>

## Status

Pre-release. Exercises D-STAR only. Public API is unstable.

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
