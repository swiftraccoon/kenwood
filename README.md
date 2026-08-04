# kenwood

[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-github.io-blue)](https://swiftraccoon.github.io/kenwood/)

[![Ubuntu](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/ubuntu.yml?label=Ubuntu&logo=ubuntu)](https://github.com/swiftraccoon/kenwood/actions/workflows/ubuntu.yml)
[![Fedora](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/fedora.yml?label=Fedora&logo=fedora)](https://github.com/swiftraccoon/kenwood/actions/workflows/fedora.yml)
[![macOS](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/macos.yml?label=macOS&logo=apple)](https://github.com/swiftraccoon/kenwood/actions/workflows/macos.yml)
[![Windows](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/windows.yml?label=Windows&logo=windows)](https://github.com/swiftraccoon/kenwood/actions/workflows/windows.yml)

[![Security](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/security.yml?label=Security&logo=githubactions)](https://github.com/swiftraccoon/kenwood/actions/workflows/security.yml)
[![codecov](https://codecov.io/gh/swiftraccoon/kenwood/graph/badge.svg?token=TVW1UKPRMW)](https://codecov.io/gh/swiftraccoon/kenwood)

Rust workspace for Kenwood amateur-radio transceivers: core TH-D75 library, TUI, accessible REPL, plus generic packet-radio, D-STAR reflector, DMR network, and MMDVM protocol stacks usable independently of any specific radio.

> **Hardware risk.** This code talks to real radios over CAT, MCP, KISS, and MMDVM. Incorrect memory writes can corrupt radio configuration. Do not use this on a radio you are not prepared to factory-reset or send in for service. No warranty; see [LICENSE](LICENSE).

> **API instability.** Every crate in this workspace is pre-1.0 and pre-release. Public APIs change without notice, often within a single commit. Nothing here is published to crates.io. Pin to a specific git SHA if you need a stable build. `Cargo.lock` is gitignored.


## Radios

| Radio | Core | TUI | REPL | Status |
|-------|------|-----|------|--------|
| TH-D75A/E | [`thd75/`](thd75/) | [`thd75-tui/`](thd75-tui/) | [`thd75-repl/`](thd75-repl/) | experimental |
| TM-D750 | Planned | Planned | Planned | not started |

## Crates

Every crate in this workspace is `experimental`: may not compile on
all platforms, under heavy change, public API and wire formats can
break between any two commits. Do not depend on anything here
expecting stability.

| Crate | Purpose | Stability |
|-------|---------|-----------|
| [`thd75/`](thd75/) | TH-D75 library: CAT, MCP programming, SD-card parsing, transports, high-level `AprsClient` | experimental |
| [`thd75-tui/`](thd75-tui/) | Terminal UI for the TH-D75 | experimental |
| [`thd75-repl/`](thd75-repl/) | Screen-reader-friendly REPL (CAT, APRS, D-STAR gateway) | experimental |
| [`thd75-listen/`](thd75-listen/) | Accessible SSB/CW/AM listener pipeline for the TH-D75's IF-over-USB-audio stream; its executable cannot currently start a session because mandatory initial tuning hits the fail-closed FO/FQ write quarantine | experimental |
| [`if-dsp/`](if-dsp/) | Sans-io DSP for a 12 kHz low-IF stream: channelizer, USB/LSB/CW/AM demodulation, AGC | experimental |
| [`mcp-d75-extract/`](mcp-d75-extract/) | Generates the TH-D75 menu manifest and Rust field registry from the official programming software | experimental |
| [`kiss-tnc/`](kiss-tnc/) | KISS TNC wire framing (`no_std` + `alloc`, sans-io) | experimental |
| [`ax25-codec/`](ax25-codec/) | AX.25 v2.2 frame codec (`no_std` + `alloc`, sans-io) | experimental |
| [`aprs/`](aprs/) | APRS parser, digipeater, SmartBeaconing, messaging, station list (std, sans-io) | experimental |
| [`aprs-is/`](aprs-is/) | APRS-IS TCP client (tokio) | experimental |
| [`dstar-gateway-core/`](dstar-gateway-core/) | DPlus / DExtra / DCS reflector codecs + typestate client and server sessions (no I/O) | experimental |
| [`dstar-gateway/`](dstar-gateway/) | Tokio async shell over `dstar-gateway-core` with auth and host-file fetcher | experimental |
| [`dstar-gateway-server/`](dstar-gateway-server/) | Multi-client D-STAR reflector server with cross-protocol forwarding; ships a `polaris` binary as a standalone local DExtra test reflector | experimental |
| [`dmr-rewind-core/`](dmr-rewind-core/) | Sans-io codec and authentication primitives for BrandMeister Open DMR Terminal | experimental |
| [`dmr-rewind/`](dmr-rewind/) | Receive-only Tokio client for self-service BrandMeister Open DMR Terminal subscriptions | experimental |
| [`pulsar/`](pulsar/) | Software-only DMR recorder: archives BrandMeister AMBE+2 bursts and per-call metadata | experimental |
| [`mmdvm-core/`](mmdvm-core/) | Sans-io MMDVM modem protocol codec | experimental |
| [`mmdvm/`](mmdvm/) | Tokio async shell for MMDVM modems | experimental |
| [`mbelib-rs/`](mbelib-rs/) | AMBE 3600×2400 voice codec (decoder default; encoder behind `--features encoder`) | experimental |
| [`sextant/`](sextant/) | GUI D-STAR reflector client; exercises the laptop-only encode/decode pipeline against a local `polaris` reflector | experimental |
| [`stargazer/`](stargazer/) | D-STAR reflector recorder and activity survey: listen-only multi-target capture to raw AMBE, WAV, and metadata | experimental |
| [`lodestar-core/`](lodestar-core/) | UniFFI Rust core for the Lodestar macOS/iPadOS app | experimental |
| [`azimuth-core/`](azimuth-core/) | UniFFI Rust core for Azimuth: authenticated automation, stale-safe MCP settings, host-owned APRS/KISS, and physical IF-DSP | experimental |

## App

| App | Platform | Source | Status |
|-----|----------|--------|--------|
| Lodestar | iPadOS, macOS | [`lodestar/`](lodestar/) (Xcode) + [`lodestar-core/`](lodestar-core/) (Rust via UniFFI) | experimental |
| Azimuth | M-series iPadOS, macOS | [`azimuth/`](azimuth/) (Xcode) + [`azimuth-core/`](azimuth-core/) (Rust via UniFFI) | experimental |

Lodestar is a SwiftUI D-STAR gateway app for DPlus / DExtra / DCS reflectors. The macOS build bridges a TH-D75 over native `IOBluetooth` RFCOMM; the iPadOS build drives the radio directly over USB-C through an embedded DriverKit extension on M-series iPads, and keeps relaying while backgrounded. Both also work reflector-only with no radio (TX/RX over IP). iPhone is not supported. Build via XcodeGen: `(cd lodestar && xcodegen generate && open Lodestar.xcodeproj)`.

Azimuth is the dedicated SwiftUI TH-D75 control center and field guide. It mirrors the authenticated 240×180 radio screen, exposes all 25 guarded front-panel keys, reads the complete reviewed 400-record settings catalog, and applies user-approved natural-language proposals through stale-safe verified MCP transactions. Its operational workspaces continuously monitor host-owned APRS/KISS activity and provide explicitly confirmed one-shot message and position transmission. On iPadOS, IF-DSP captures the current Band B VFO frequency for USB/LSB/CW/AM analysis, spectrum, waterfall, and level diagnostics; direct retuning remains unavailable while FO/FQ frequency writes fail closed. Azimuth uses direct USB-C through an embedded DriverKit extension on M-series iPads and the public CDC serial device on macOS. Build the core with `./azimuth-core/scripts/build-xcframework.sh`, then generate the app with `(cd azimuth && xcodegen generate)`.

## Building

```
cargo build --workspace
cargo test --workspace
./lint.sh       # unsafe audit, clippy --all-targets, nextest + doctests, docs, fmt, audit, deny, machete, shellcheck, taplo, mdbook
./ci-local.sh   # cross-platform CI: macOS locally, Ubuntu + Fedora in k8s pods
```

Rust 1.94+, edition 2024. Workspace-level lints enforce `unsafe_code = "forbid"`, `missing_docs = "deny"`, and clippy `pedantic`/`nursery`/`cargo`. A crate's `[lints]` table replaces the workspace one rather than merging, so five crates restate it: `thd75` denies (rather than forbids) `unsafe_code` only for the audited macOS `IOBluetooth` helper and Vision OCR bridge FFI, `thd75-tui` forbids it, `lodestar-core` and `azimuth-core` permit generated UniFFI scaffolding, and `thd75-repl` restates the table only to diverge on other lints; it forbids `unsafe_code` and contains no FFI. Every `unsafe` block outside that allowlist is rejected by `./lint.sh`'s unsafe audit.

## License

GPL-2.0-or-later.

Derived works and attribution:

- [`mmdvm/`](mmdvm/) and [`mmdvm-core/`](mmdvm-core/): portions derived from [MMDVMHost](https://github.com/g4klx/MMDVMHost) by Jonathan Naylor G4KLX (2015–2026, GPL-2.0-or-later).
- [`dstar-gateway-core/`](dstar-gateway-core/) reflector codec constants and session-transition timing are derived from [ircDDBGateway](https://github.com/g4klx/ircDDBGateway) by Jonathan Naylor G4KLX (GPL-2.0-or-later) and [xlxd](https://github.com/LX3JL/xlxd) by LX3JL and contributors (GPL-2.0-or-later).
- [`dmr-rewind-core/`](dmr-rewind-core/) independently implements the public BrandMeister REWIND wire format, cross-checked against BrandMeister's [go-brandmeister](https://github.com/BrandMeister/go-brandmeister), [DigestPlay](https://github.com/BrandMeister/DigestPlay), and [callrec](https://github.com/BrandMeister/callrec) interoperability references.
- [`mbelib-rs/`](mbelib-rs/): Rust port of [mbelib](https://github.com/szechyjs/mbelib) and [DSD](https://github.com/szechyjs/dsd) by szechyjs (originally ISC-licensed; redistributed here under GPL-2.0-or-later per ISC's relicensing allowance). Relicensing pathway follows [mbelib-neo](https://github.com/arancormonk/mbelib-neo) by arancormonk.
- [`aprs/`](aprs/) SmartBeaconing implementation: algorithm by Tony Arnerich KD7TA and Steve Bragg KA9MVA (HamHUD).
- KISS protocol specification (Chepponis / Karn, 1987) and AX.25 v2.2 (TAPR, 1998) are referenced as public specifications, not derivations.
