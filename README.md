# kenwood

[![Rust](https://img.shields.io/badge/rust-1.89%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](LICENSE)
[![Docs](https://img.shields.io/badge/docs-github.io-blue)](https://swiftraccoon.github.io/kenwood/)

[![Ubuntu](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/ubuntu.yml?label=Ubuntu&logo=ubuntu)](https://github.com/swiftraccoon/kenwood/actions/workflows/ubuntu.yml)
[![Fedora](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/fedora.yml?label=Fedora&logo=fedora)](https://github.com/swiftraccoon/kenwood/actions/workflows/fedora.yml)
[![macOS](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/macos.yml?label=macOS&logo=apple)](https://github.com/swiftraccoon/kenwood/actions/workflows/macos.yml)
[![Windows](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/windows.yml?label=Windows&logo=windows)](https://github.com/swiftraccoon/kenwood/actions/workflows/windows.yml)

[![Security](https://img.shields.io/github/actions/workflow/status/swiftraccoon/kenwood/security.yml?label=Security&logo=githubactions)](https://github.com/swiftraccoon/kenwood/actions/workflows/security.yml)
[![codecov](https://codecov.io/gh/swiftraccoon/kenwood/graph/badge.svg?token=TVW1UKPRMW)](https://codecov.io/gh/swiftraccoon/kenwood)

Rust libraries, TUI, and accessible REPL for Kenwood amateur radio transceivers.

> **WARNING: This project is a work in progress. Use at your own risk. There are no guarantees that this software will not damage, brick, or otherwise render your radio inoperable. Incorrect memory writes can corrupt radio configuration. Do not use this on a radio you are not prepared to factory reset or send in for service.**

![thd75-tui](thd75_tui.png)

## Radios

| Radio | Library | TUI | REPL | Status |
|-------|---------|-----|------|--------|
| TH-D75 | `thd75/` | `thd75-tui/` | `thd75-repl/` | In development |
| TM-D750 | Planned | Planned | Planned | Not started |

## Accessible REPL

`thd75-repl` is a screen-reader-friendly command-line interface for blind and visually impaired operators. It follows WCAG 2.1 accessibility guidelines and the CHI 2021 CLI accessibility study recommendations:

- Plain text output, one self-contained line per datum
- Natural language units ("146.52 megahertz", not "146520000 Hz")
- All abbreviations expanded on first use
- Consistent "Error:" prefix for screen reader search
- No box drawing, escape sequences, spinners, or cursor repositioning
- Three operating modes: CAT (radio control), APRS (packet radio), D-STAR (digital voice gateway)

```
$ thd75-repl
Kenwood TH-D75 accessible radio control, version 0.1.0.
Connected via bluetooth:TH-D75.
Radio is in D-STAR Reflector Terminal Mode.
Type dstar start <callsign> to begin, or quit to exit.
d75> dstar start KQ4NIT
Checking if radio is already in D-STAR gateway mode.
Radio is already in Reflector Terminal Mode.
Starting D-STAR gateway as KQ4NIT.
MMDVM modem initialized.
No reflector specified. Use link command to connect.
D-STAR gateway active. Type dstar stop to exit.
Commands: listen, link, unlink, heard, status, dstar stop
dstar> link REF030C
Connecting to REF030 module C at 45.79.43.161:20001.
Authenticating with D-STAR gateway server.
Authentication successful.
Waiting for reflector acknowledgement.
Connected to REF030 module C.
dstar> monitor
Entering monitor mode. Press Ctrl-C to return to prompt.
Reflector: voice from K3ATA to CQCQCQ.
Reflector: voice transmission ended.
Reflector: voice from KF0WFM to CQCQCQ.
Reflector: voice transmission ended.
```
