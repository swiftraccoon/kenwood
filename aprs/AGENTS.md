# aprs

Sans-io, std-only. **This crate never calls `Instant::now()`**: every stateful
method takes `now: Instant`. The clock sources are the tokio shell (`aprs-is`)
and the thd75 integration. The layering rule is in the root `AGENTS.md` section
`Workspace boundaries`.

```bash
cargo nextest run -p aprs --test campaign_reference --no-capture  # print pinned on-air reference payloads for APRS-IS comparison
```

Every `tests/*.rs` needs a `use <dep> as _;` line for each dependency it does not
otherwise use, because `unused_crate_dependencies` fires per compilation unit.
`src/build.rs` is the outgoing-builder module, NOT a Cargo build script.

## Clock-parameter exceptions

- `AprsMessenger::send_message` and `cleanup_expired` bind `_now`: the parameter is accepted for API consistency and is currently unused. Pending-message expiry is retry-count driven (`attempts >= max_retries`), not deadline driven.
- `AprsMessenger::process_incoming` takes no `now` at all. Pair it with `is_new_incoming`, which does.

## Typed boundary

- Builders accept an `&Ax25Address` source and an `&[RouteEntry]` path. No raw-string construction at call sites. The Mic-E destination is built infallibly from `Callsign::new` plus `Ax25Address::from_parts`; the 6 latitude-encoded bytes are valid by construction, and the function panics rather than substituting a fallback if that invariant is ever broken.
- `PhgDirectivity` is a typed enum, not a raw `u16`: cardinal and intercardinal variants plus `Omni` and `Undefined` (spec code 9). Recover the bearing with `to_degrees() -> Option<u16>` and gate the range circle on `is_omni()`.
- `AprsMessenger::new` takes `Ax25Address` plus the validated `DigipeaterPath` newtype. The only remaining `&str` boundary is `is_new_incoming`'s `source`.

## Parsing and builder rules

- `parse_weather_fields` is private to avoid a tag-collision footgun on non-weather data.
- `parse_aprs_data_full(info, destination)` handles Mic-E; `parse_aprs_data(info)` returns `MicERequiresDestination` for Mic-E. Mic-E speed and course decode from `info[4..7]`.
- `build_aprs_position_report*` and `build_aprs_position_compressed*` clamp latitude to +/-90 and longitude to +/-180 and substitute `0.0` for non-finite inputs. This prevents silent `as u32` saturation producing a valid-looking but wildly wrong wire encoding.
- `build_aprs_mice*` clamps the longitude-hundredths roundup to `0..=99` to keep the wire byte inside the Mic-E receivable range of `28..=127`.

Builders emit KISS-wrapped wire bytes through `kiss-tnc`; that is the only
cross-crate path dependency in this stack besides `ax25-codec`.
