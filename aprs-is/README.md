# aprs-is

APRS-IS (APRS Internet Service) TCP client. Tokio shell, depends on [`aprs`](../aprs/) (and directly on [`ax25-codec`](../ax25-codec/) for q-construct path rewriting).

## Scope

- `AprsIsClient`: connect (with retry), login, 120 s keepalive, next-event loop, send-packet, shutdown.
- `AprsIsConfig`: callsign, passcode, server, port, optional filter, software-name and -version strings. (Connect timeout and keepalive interval are module constants `CONNECT_TIMEOUT` / `KEEPALIVE_INTERVAL`, not config fields.)
- `aprs_is_passcode(callsign: &str) -> i32`: the standard two-byte-XOR hash.
- `build_login_string(&AprsIsConfig) -> Result<String, AprsIsError>`: `user <call> pass <code> vers <sw> <ver> [filter <spec>]` (errors on login fields with whitespace/control characters, or a filter containing `CR`/`LF`).
- `AprsIsFilter`: a `#[non_exhaustive]` enum covering the APRS-IS filter clause types (range, area, prefix, budlist, object, strict-object, type, type-around, symbol, digi, entry, q-construct, my-range, group, friend, unproto, negation, and raw). `as_wire()` renders one clause; `join()` concatenates a slice into a filter string; `negated()` / `raw()` construct the negation and passthrough variants.
- `QConstruct`: qAC / qAX / qAU / qAo / qAO / qAS / qAr / qAR / qAZ / qAI classification, `format_is_packet_with_qconstruct` for basic q-construct appending, and `igate_format_for_is` for the full q.aspx IGate path-rewriting algorithm.
- `AprsIsLine` + `parse_is_line`: TNC2 monitor-format parser.
- `format_is_packet`: outbound line formatter.

## Scope boundaries

APRS parsing is in [`aprs`](../aprs/). This crate is the network adapter only; it does not decode APRS info fields itself, and callers hand `AprsIsLine::parse`'d output to the `aprs` parser.

## Status

Extracted from `kenwood-thd75` April 2026. Pre-release. Public API is unstable. There is no *automatic* reconnect: on an idle disconnect the client surfaces an `AprsIsEvent::Disconnected` event, and callers reconnect explicitly via `AprsIsClient::reconnect()` or by establishing a fresh connection with `connect_with_retry()`. The default `software_name` is `"aprs-is"` and should be overridden per integration (e.g. `kenwood-thd75` sets it explicitly).

## References

- APRS-IS: <http://www.aprs-is.net/>
- Q-construct rules: <http://www.aprs-is.net/q.aspx>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
