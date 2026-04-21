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

Rust workspace for Kenwood amateur-radio transceivers: core TH-D75 library, TUI, accessible REPL, plus generic packet-radio, D-STAR reflector, and MMDVM protocol stacks usable independently of any specific radio.

> **Hardware risk.** This code talks to real radios over CAT, MCP, KISS, and MMDVM. Incorrect memory writes can corrupt radio configuration. Do not use this on a radio you are not prepared to factory-reset or send in for service. No warranty; see [LICENSE](LICENSE).

> **API instability.** Every crate in this workspace is pre-1.0 and pre-release. Public APIs change without notice — often within a single commit. Nothing here is published to crates.io. Pin to a specific git SHA if you need a stable build. `Cargo.lock` is gitignored.


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
| [`kiss-tnc/`](kiss-tnc/) | KISS TNC wire framing (`no_std` + `alloc`, sans-io) | experimental |
| [`ax25-codec/`](ax25-codec/) | AX.25 v2.2 frame codec (`no_std` + `alloc`, sans-io) | experimental |
| [`aprs/`](aprs/) | APRS parser, digipeater, SmartBeaconing, messaging, station list (std, sans-io) | experimental |
| [`aprs-is/`](aprs-is/) | APRS-IS TCP client (tokio) | experimental |
| [`dstar-gateway-core/`](dstar-gateway-core/) | DPlus / DExtra / DCS reflector codecs + typestate client and server sessions (no I/O) | experimental |
| [`dstar-gateway/`](dstar-gateway/) | Tokio async shell over `dstar-gateway-core` with auth and host-file fetcher | experimental |
| [`dstar-gateway-server/`](dstar-gateway-server/) | Multi-client D-STAR reflector server with cross-protocol forwarding; ships a `polaris` binary as a standalone local DExtra test reflector | experimental |
| [`mmdvm-core/`](mmdvm-core/) | Sans-io MMDVM modem protocol codec | experimental |
| [`mmdvm/`](mmdvm/) | Tokio async shell for MMDVM modems | experimental |
| [`mbelib-rs/`](mbelib-rs/) | AMBE 3600×2400 voice codec (decoder default; encoder behind `--features encoder`) | experimental |
| [`sextant/`](sextant/) | GUI D-STAR reflector client — exercises the laptop-only encode/decode pipeline against a local `polaris` reflector | experimental |
| [`stargazer/`](stargazer/) | Tracing / metrics / observability scaffolding | experimental |

## Building

```
cargo build --workspace
cargo test --workspace
./lint.sh       # clippy --all-targets, cargo-audit, cargo-deny, cargo-machete, fmt
./ci-local.sh   # cross-platform CI in macOS + Ubuntu + Fedora pods
```

Rust 1.94+, edition 2024. Workspace-level lints enforce `unsafe_code = "forbid"`, `missing_docs = "deny"`, and clippy `pedantic`/`nursery`/`cargo` across every crate.

## License

GPL-2.0-or-later.

Derived works and attribution:

- [`mmdvm/`](mmdvm/) and [`mmdvm-core/`](mmdvm-core/) — portions derived from [MMDVMHost](https://github.com/g4klx/MMDVMHost) by Jonathan Naylor G4KLX (2015–2026, GPL-2.0-or-later).
- [`dstar-gateway-core/`](dstar-gateway-core/) reflector codec constants and session-transition timing — derived from [ircDDBGateway](https://github.com/g4klx/ircDDBGateway) by Jonathan Naylor G4KLX (GPL-2.0-or-later) and [xlxd](https://github.com/LX3JL/xlxd) by LX3JL and contributors (GPL-2.0-or-later).
- [`mbelib-rs/`](mbelib-rs/) — Rust port of [mbelib](https://github.com/szechyjs/mbelib) and [DSD](https://github.com/szechyjs/dsd) by szechyjs (originally ISC-licensed; redistributed here under GPL-2.0-or-later per ISC's relicensing allowance). Relicensing pathway follows [mbelib-neo](https://github.com/arancormonk/mbelib-neo) by arancormonk.
- [`aprs/`](aprs/) SmartBeaconing implementation — algorithm by Tony Arnerich KD7TA and Steve Bragg KA9MVA (HamHUD).
- KISS protocol specification (Chepponis / Karn, 1987) and AX.25 v2.2 (TAPR, 1998) are referenced as public specifications, not derivations.
