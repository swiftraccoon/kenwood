# ax25-codec

AX.25 v2.2 frame encode/decode. `no_std` + `alloc`, sans-io.

## Scope

- `Ax25Packet` — source, destination, up to 8 digipeaters, control field, PID, info payload.
- `parse_ax25(&[u8]) -> Result<Ax25Packet, Ax25Error>` and `build_ax25(&Ax25Packet) -> Vec<u8>`.
- `Ax25Control` classification: Information / Supervisory{RR, RNR, REJ, SREJ} / Unnumbered{UI, SABM, DISC, DM, UA, FRMR, XID, TEST, ...}.
- `Ax25Pid` enum with 15 canonical values (NoLayer3, NetRom, Ip, Arp, Appletalk, TexNet, LinkQuality, FlexNet, ...).
- `CommandResponse` classification per AX.25 v2.2 §4.3.1.2.
- `ax25_fcs(&[u8]) -> u16` CRC-16-CCITT.
- Validated address types: `Ax25Address`, `Callsign` (1–6 ASCII upper/digit), `Ssid` (0–15).

This is the AX.25 layer only. KISS framing is in [`kiss-tnc`](../kiss-tnc/); APRS info-field parsing is in [`aprs`](../aprs/).

## Status

Extracted from `kenwood-thd75` April 2026. Pre-release. Public API is unstable. `Callsign`/`Ssid` error paths currently thread through `Ax25Error`; that boundary may shift as additional consumers land.

## Reference

AX.25 v2.2 specification: <http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
