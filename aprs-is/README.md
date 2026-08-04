# aprs-is

APRS-IS (APRS Internet Service) TCP client. Tokio shell, depends on [`aprs`](../aprs/) (and directly on [`ax25-codec`](../ax25-codec/) for q-construct path rewriting).

## Scope

- `AprsIsClient`: connect (with retry), login, 120 s keepalive, next-event loop, text packet sending, byte-preserving RF uplink sending, and shutdown.
- `AprsIsConfig`: callsign, passcode, server, port, optional filter, software-name and -version strings. (Connect timeout and keepalive interval are module constants `CONNECT_TIMEOUT` / `KEEPALIVE_INTERVAL`, not config fields.)
- `aprs_is_passcode(callsign: &str) -> i32`: the standard two-byte-XOR hash.
- `build_login_string(&AprsIsConfig) -> Result<String, AprsIsError>`: `user <call> pass <code> vers <sw> <ver> [filter <spec>]` (enforces the APRS-IS callsign/SSID grammar, one-word software fields, and a filter without `CR`/`LF`).
- `AprsIsFilter`: a `#[non_exhaustive]` enum covering the APRS-IS filter clause types (range, area, prefix, budlist, object, strict-object, type, type-around, symbol, digi, entry, q-construct, my-range, group, friend, unproto, negation, and raw). `as_wire()` renders one clause; `join()` concatenates a slice into a filter string; `negated()` / `raw()` construct the negation and passthrough variants.
- `QConstruct`: qAC / qAX / qAU / qAo / qAO / qAS / qAr / qAR / qAZ / qAI classification, `format_is_packet_with_qconstruct` for validated text-only q-construct appending, and `igate_format_for_is` for the full q.aspx IGate path-rewriting algorithm. Both formatters return `AprsIsUplinkLine`; the strict IGate formatter also preserves binary information bytes and normalizes only terminal TNC CR/LF delimiters into APRS-IS framing.
- `AprsIsUplinkLine`: validated byte-native wire line with exactly one trailing CRLF, no embedded CR/LF, and a 512-byte maximum including framing. `AprsIsClient::send_uplink_line` accepts this proof type; `send_raw_bytes` validates existing wire bytes before sending them unchanged.
- `AprsIsLine`: strict, byte-preserving TNC2 packet parser. Source,
  destination, and route elements use APRS-IS-native alphanumeric identity
  types (including one- or two-character SSIDs and nine-byte server logins);
  Q-constructs have their own enum variant; embedded line framing and malformed
  identities are rejected with `AprsIsLineError`.
- `parse_is_line`: lightweight server-line classifier for the socket event
  loop (comments/keepalives versus packet candidates).
- `format_is_packet`: text-only outbound builder returning a validated `AprsIsUplinkLine`. Use `igate_format_for_is` for RF-to-IS path rewriting and byte-native information fields.

## Scope boundaries

APRS information-field decoding is in [`aprs`](../aprs/). This crate validates
the APRS-IS/TNC2 envelope without interpreting the information bytes. The
strict parser retains those bytes exactly, including non-UTF-8 Mic-E data, so a
caller can pass them to `aprs` or construct a byte-exact third-party RF frame.

## Status

Extracted from `kenwood-thd75` April 2026. Pre-release. Public API is unstable. There is no *automatic* reconnect: on an idle disconnect the client surfaces an `AprsIsEvent::Disconnected` event, and callers reconnect explicitly via `AprsIsClient::reconnect()` or by establishing a fresh connection with `connect_with_retry()`. The default `software_name` is `"aprs-is"` and should be overridden per integration (e.g. `kenwood-thd75` sets it explicitly).

## References

- APRS-IS: <https://www.aprs-is.net/>
- Q-construct rules: <https://www.aprs-is.net/q.aspx>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
