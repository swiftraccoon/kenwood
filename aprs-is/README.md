# aprs-is

APRS-IS (APRS Internet Service) TCP client. Tokio shell, depends on [`aprs`](../aprs/).

## Scope

- `AprsIsClient` — connect (with retry), login, 120 s keepalive, next-event loop, send-packet, shutdown.
- `AprsIsConfig` — callsign, passcode, optional filter, connect / keepalive timeouts, software-name and -version strings.
- `aprs_is_passcode(callsign: &str) -> i32` — the standard two-byte-XOR hash.
- `build_login_string(&AprsIsConfig) -> String` — `user <call> pass <code> vers <sw> <ver> [filter <spec>]`.
- `AprsIsFilter` — builder for range, area, prefix, budlist, object, type, symbol, friend, group, and raw filter strings.
- `QConstruct` — qAC / qAX / qAU / qAo / qAO / qAS / qAr / qAR / qAZ classification and `format_is_packet_with_qconstruct` for IGate path rewriting.
- `AprsIsLine` + `parse_is_line` — TNC2 monitor-format parser.
- `format_is_packet` — outbound line formatter.

## Scope boundaries

APRS parsing is in [`aprs`](../aprs/). This crate is the network adapter only — it does not decode APRS info fields itself; callers hand `AprsIsLine::parse`'d output to the `aprs` parser.

## Status

Extracted from `kenwood-thd75` April 2026. Pre-release. Public API is unstable. The client does not yet reconnect on idle disconnect; callers drive reconnect logic via `next_event` error handling. The default `software_name` is `"aprs-is"` and should be overridden per integration (e.g. `kenwood-thd75` sets it explicitly).

## References

- APRS-IS: <http://www.aprs-is.net/>
- Q-construct rules: <http://www.aprs-is.net/q.aspx>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
