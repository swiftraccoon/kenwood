# lodestar-core

Rust core for [Lodestar](../lodestar) — the iOS, iPadOS, and Mac Catalyst
D-STAR gateway app for the Kenwood TH-D75.

Wraps the existing workspace crates (`dstar-gateway-core`, `dstar-gateway`,
`kenwood-thd75`) and exposes them to Swift via [UniFFI](https://mozilla.github.io/uniffi-rs/).
The Swift bindings ship as `LodestarKit.xcframework`, produced by
`scripts/build-xcframework.sh`.

## Phase 1 scope

Only `version()` is exposed. See `docs/superpowers/plans/2026-04-20-lodestar-phase-1-foundation.md`
(gitignored) for the build-up schedule.

## License

GPL-2.0-or-later OR GPL-3.0-or-later.
