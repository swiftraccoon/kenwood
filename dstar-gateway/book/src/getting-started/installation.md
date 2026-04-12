# Installation & feature flags

Add `dstar-gateway` to your `Cargo.toml`:

```toml
[dependencies]
dstar-gateway = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "macros"] }
```

This gets you the default feature set: the tokio-backed
`AsyncSession<P>`, the DPlus `AuthClient`, and all three codecs.
`dstar-gateway-core` is pulled in transitively — you don't need
to list it as a separate dependency.

## Feature flags

| Feature | Default? | What it enables |
|---------|----------|-----------------|
| `blocking` | off | Compiles a non-tokio blocking shell under `dstar_gateway::blocking_shell`. Useful for CLI scripts and test fixtures that don't want to spin up a tokio runtime. |
| `hosts-fetcher` | off | Pulls `reqwest` and enables `dstar_gateway::hosts_fetcher::HostsFetcher`, which downloads the Pi-Star `DPlus_Hosts.txt` / `DExtra_Hosts.txt` / `DCS_Hosts.txt` files over HTTPS. |

To enable both:

```toml
[dependencies]
dstar-gateway = { version = "0.1", features = ["blocking", "hosts-fetcher"] }
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
dstar-gateway-server = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "macros", "sync"] }
```

The server crate re-exports the common types from
`dstar-gateway-core`, so you don't need to depend on either of
the other two crates unless you specifically want the client
`AsyncSession<P>` as well (e.g. to implement a cross-protocol
bridge that is both a client and a server).

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

    // We don't actually connect — this just verifies the types resolve.
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

`dstar-gateway` ships with around **2,200 tests**, plus 10 fuzz
targets and a loopback integration test harness for every
protocol. The full test suite runs in under a minute on a modern
laptop:

```bash
cargo test --workspace
```

Fuzz targets live under `dstar-gateway-core/fuzz/` and run nightly
in CI. You can run one locally with:

```bash
cd dstar-gateway-core/fuzz
cargo +nightly fuzz run parse_dplus_header -- -max_total_time=60
```

Once you've verified everything builds, move on to
[Hello, REF030 (DPlus)](hello-dplus.md) or one of the other two
walkthroughs.
