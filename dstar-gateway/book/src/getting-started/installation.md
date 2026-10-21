# Installation and feature flags

The packages are unpublished. Select a repository revision or local path:

```toml
[dependencies]
dstar-gateway = { git = "https://github.com/swiftraccoon/kenwood", rev = "<commit SHA>" }
dstar-gateway-core = { git = "https://github.com/swiftraccoon/kenwood", rev = "<commit SHA>" }
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "macros"] }
```

The executable baseline is Rust 1.94.0 and edition 2024. The core dependency
provides Session, protocol/state markers and checked transmit values; the
client shell supplies the network driver.

| Feature | Default | Contract |
| --- | --- | --- |
| blocking | off | Synchronous connect/event/TX/disconnect driver over an owned std UDP socket. No running Tokio runtime is needed. |
| insecure-plaintext-xlx-directory | off | Optional reqwest client for the selected plaintext HTTP endpoint. Total time/body limits and structural host validation do not authenticate returned addresses. |
| examples-network | off | Compiles examples that require external network services when run. |
| hardware-tests | off | Compiles ignored live-reflector tests. Voice TX additionally requires DSTAR_TEST_TX_OK=1. |

Enable features in the dependency's features array. Directory-client new is
fallible and accepts custom client/limits; authentication defaults and resource
contracts are documented in the published README.

To compile maintained examples without opening sockets:

```bash
cargo check -p dstar-gateway --all-targets --all-features
```

The [local DPlus walkthrough](hello-dplus.md) includes its checked example
source. The
[blocking example](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/examples/09_blocking_cli.rs)
uses the synchronous shared handshake and treats idle None separately from
closure. The
[DPRS listener](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/examples/06_dprs_position_listener.rs)
reconstructs bounded sentences before parsing.

For a reflector, depend on dstar-gateway-server and dstar-gateway-core.
The server README describes configuration, authorization, run/shutdown and
caller-visible outcomes; the client shell is a separate dependency.

Full-tree validation follows the workspace
[contributor workflow](https://github.com/swiftraccoon/kenwood/blob/main/CONTRIBUTING.md).
