# aprs

APRS (Automatic Packet Reporting System) protocol stack. std-only, sans-io.

## Scope

**Parsers.** Position (uncompressed + compressed), Mic-E (with speed/course from `dest` bytes + `info[4..7]`), weather (positionless and position-embedded, CSE/SPD + 13-field), status, telemetry, messages, items, objects, queries, third-party, grid square, raw GPS, user-defined.

**Builders.** `build_aprs_item`, `build_aprs_message`, `build_aprs_mice`, `build_aprs_object`, `build_aprs_position_compressed`, `build_aprs_position_report`, `build_aprs_status`, `build_aprs_weather`, `build_query_response_position`, plus `_packet` variants returning `Ax25Packet`.

**Stateful algorithms.**

- `DigipeaterConfig` — path handling, 30 s dup cache keyed on `(source, dest, info)` hash, own-callsign loop detection, optional viscous delay queue.
- `SmartBeaconing` — HamHUD (Tony Arnerich KD7TA / Steve Bragg KA9MVA) formula with low/high speeds, turn slope, turn minimum, turn time.
- `AprsMessenger` — ack/rej classification via strict `^(ack|rej)[A-Za-z0-9]{1,5}$`, per-message retry backoff, incoming dedup window.
- `StationList` — heard-station database with expiry and bounded capacity.

**Validated newtypes.** `Latitude`, `Longitude`, `Speed`, `Course`, `MessageId`, `SymbolTable`, `AprsSymbol`, `Fahrenheit`, `Tocall`.

## Time handling

Every stateful method accepts `now: Instant` as a parameter. The crate never calls `std::time::Instant::now()` itself. Tokio shells (e.g. the `AprsClient` in [`kenwood-thd75`](../thd75/)) read the wall clock once per iteration and thread it downward. This keeps the crate deterministic under test and enables synthetic-clock replay of captured streams.

## Scope boundaries

- AX.25 framing: [`ax25-codec`](../ax25-codec/).
- KISS envelope: [`kiss-tnc`](../kiss-tnc/). `build_aprs_*` helpers produce KISS-wrapped wire bytes via an `ax25_to_kiss_wire` adapter; the `_packet` variants return a bare `Ax25Packet` instead.
- APRS-IS TCP client: [`aprs-is`](../aprs-is/).

## Status

Extracted from `kenwood-thd75` April 2026. Pre-release. Public API is unstable — `StationList::update` currently takes `&str` / `&[String]` pending planned tightening to `&Callsign` / `&[Ax25Address]`. Error-variant names may change. The `aprs` → `kiss-tnc` dependency (for `ax25_to_kiss_wire`) may be removed in a future reorganization that pushes KISS wrapping back up to the consumer.

## References

- APRS 1.0.1: <http://www.aprs.org/doc/APRS101.PDF>
- APRS 1.2 addenda: <http://www.aprs.org/aprs12.html>

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
