# sextant

Desktop egui D-STAR reflector client. No radio hardware.

`sextant` is a bin-only package: never pass `--lib` to a scoped
`cargo doc -p sextant`, it fails with no library to document. The full gate's
`--lib --bins` selection is a workspace-level invocation, not a per-crate one.

## Rules

- The audio worker runs on a dedicated `std::thread` so the tokio runtime never owns a `cpal::Stream`, which is `!Send` on some platforms.
- Resampling is `rubato`'s windowed sinc, which bandlimits as it resamples: there is no separate anti-alias filter to add.
- ALL colors come from `theme.rs` consts; never inline `Color32::from_rgb` in a panel. Color discipline: `LIVE` amber for live RF only, `TX` red for on-air only, `OK` green for link health and meters only.
- At most one `Overlay` is open at a time; model overlay visibility as that enum, never as per-overlay bools (`struct_excessive_bools` counts every bool in `App`).
- Heard history uses UTC wall-clock timestamps, not `Instant`, so the persisted file survives restarts. The debug event log stamps in the operator's `TimeMode`, which defaults to Local.
- Connect handshakes and TX stream-ID generation delegate to `dstar_gateway::tokio_shell` (`drive_connecting`, `fresh_stream_id`); do not hand-roll either.

## UI lint gotchas

- `#[expect(dead_code)]` for "consumed by a later task" items is per-target: an item used only by `#[cfg(test)]` tests leaves the expect unfulfilled in the test target while still needed in the bin target. Give the item a real bin-target consumer instead, or defer adding it.
- `clippy::format_collect`: build multi-line strings with `fold` plus `writeln!`, not `.map(format!).collect()`.

## Reflector directory gotcha

- Do not fetch the XLX self-registration registry: its server exposes only unauthenticated plaintext HTTP, and its generated addresses must not be trusted for connections. `hosts.rs` never reads `reflectors.tsv`, the operator-local cache of those rows; that file is untracked and is not expected in the tree.
- Pre-hardening settings may contain addresses copied from that cache. `reflector_address_epoch` gates a one-time migration that resets the current target and clears host-bearing favorites and recents while preserving unrelated settings. Do not remove or bypass that marker.
- Directory sources are the bundled POLARIS entry and the DPlus auth `HostList`, plus manual host entry. DPlus auth entries have dedup precedence and the picker shows per-row provenance.
- `hosts.rs` is hand-rolled and deliberately does not use `dstar_gateway_core::hosts::HostFile`, which lodestar-core uses. The two solve different problems: core `HostFile` parses and merges Pi-Star-style host FILES, while sextant has no file sources at all, only provenance-per-row runtime state plus the hardening epoch migration. Forcing `HostFile` underneath would add a conversion layer without deleting any policy code. New FILE-based host work still starts in core.

## License

Dual-licensed GPL-2.0-or-later OR GPL-3.0-or-later because it depends on
`mbelib-rs` with the `encoder` feature (OP25 ports are GPL-3.0+).
