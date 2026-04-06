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

Rust libraries and TUIs for Kenwood amateur radio transceivers.

> **WARNING: This project is a work in progress. Use at your own risk. There are no guarantees that this software will not damage, brick, or otherwise render your radio inoperable. Incorrect memory writes can corrupt radio configuration. Do not use this on a radio you are not prepared to factory reset or send in for service.**

![thd75-tui](thd75_tui.png)

## Radios

| Radio | Library | TUI | Status |
|-------|---------|-----|--------|
| TH-D75 | `thd75/` | `thd75-tui/` | In development |
| TM-D750 | Planned | Planned | Not started |
