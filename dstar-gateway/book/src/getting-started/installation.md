# Installation & feature flags

Add `dstar-gateway` to your `Cargo.toml`:

```toml
[dependencies]
dstar-gateway = { git = "https://github.com/swiftraccoon/kenwood", branch = "main" }
dstar-gateway-core = { git = "https://github.com/swiftraccoon/kenwood", branch = "main" }
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "macros"] }
```

The crates are not published to crates.io. For reproducible builds,
replace `branch = "main"` with `rev = "<commit SHA>"`; contributors
working in this monorepo can use `path` dependencies instead.

This gets you the default feature set: the tokio-backed
`AsyncSession<P>`, the DPlus `AuthClient`, and all three codecs.

`dstar-gateway` re-exports the core's leaf types (`Callsign`,
`Module`, `DstarHeader`, `VoiceFrame`, `Error`, …) from its crate
root, so simple programs need only this one dependency. The typestate
session machinery (`Session`, `Driver`, the `Configured` /
`Connected` state markers, and the `DPlus` / `DExtra` / `Dcs`
protocol markers) is **not** re-exported. If you drive the sans-io
core directly, add `dstar-gateway-core` as an explicit dependency.

## Feature flags

| Feature | Default? | What it enables |
|---------|----------|-----------------|
| `blocking` | off | Compiles a caller-driven synchronous shell under `dstar_gateway::blocking_shell`. It uses `std::net::UdpSocket` and does not run a tokio runtime, although tokio remains a dependency of the async crate. |
| `insecure-plaintext-xlx-directory` | off | Pulls `reqwest` and exposes `dstar_gateway::insecure_plaintext_xlx_directory::InsecurePlaintextXlxDirectoryFetcher`. Its `fetch_over_plaintext_http` method contacts the HTTP-only XLX registry without confidentiality, authenticity, or integrity. |
| `examples-network` | off | Compiles examples that contact live network services; intended for repository development. |
| `hardware-tests` | off | Compiles ignored live-reflector integration tests; intended for repository development. |

To enable both:

```toml
[dependencies]
dstar-gateway = { git = "https://github.com/swiftraccoon/kenwood", branch = "main", features = ["blocking", "insecure-plaintext-xlx-directory"] }
```

## MSRV

The MSRV is the `rust-version` field in any workspace
`Cargo.toml` (currently 1.94). CI pins the exact version via
`dtolnay/rust-toolchain`. The library uses Rust edition 2024.

## For reflector server builders

If you want to run a multi-client reflector, add the server crate
instead of (or alongside) the client crate:

```toml
[dependencies]
dstar-gateway-server = { git = "https://github.com/swiftraccoon/kenwood", branch = "main" }
dstar-gateway-core = { git = "https://github.com/swiftraccoon/kenwood", branch = "main" }
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "macros", "sync"] }
```

The server crate does not re-export the core's common types, so add
`dstar-gateway-core` explicitly for `Callsign`, `Module`, and related
configuration types. Add the client crate only when you also need
`AsyncSession<P>`.

## Verifying the install

A minimal smoke-test `main.rs`:

```rust,ignore
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(Callsign::try_from_str("W1AW")?)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer("127.0.0.1:30001".parse()?)
        .build();

    // We don't actually connect; this just verifies the types resolve.
    let _ = session;
    // AsyncSession is needed to satisfy the `use` above.
    let _: Option<AsyncSession<DExtra>> = None;
    println!("dstar-gateway imports compile");
    Ok(())
}
```

`cargo run` should print `dstar-gateway imports compile`. If you
see compile errors about missing trait imports, double-check
that your `use` statements match the ones in the example.

## What the test suite looks like

`dstar-gateway` ships with a large test suite, plus fuzz
targets and a loopback integration test harness for every
protocol. Runtime depends on the machine and enabled feature matrices;
the complete workspace lint runs several test/doc configurations:

```bash
cargo test --workspace
```

Fuzz targets live under `dstar-gateway-core/fuzz/` and run nightly
in CI. You can run one locally with:

```bash
cd dstar-gateway-core/fuzz
cargo +nightly fuzz run header_decode -- -max_total_time=60
```

Once you've verified everything builds, move on to
[Hello, local DPlus](hello-dplus.md) or one of the other two
walkthroughs.
