#!/usr/bin/env bash
# Full-workspace gate. Use workspace-level cargo invocations wherever
# possible so new crates added under `[workspace] members` are picked
# up automatically without editing this file.
set -euo pipefail

cd "$(dirname "$0")"

failed=0

run() {
    echo "── $* ──"
    if ! "$@"; then
        failed=1
    fi
}

# Unsafe code audit: `allow(unsafe_code)` must appear ONLY in the
# known-required spots. Globs cover every crate's `src/`; add new
# exclusions as deliberate design decisions, never blanket-whitelist.
echo "── unsafe audit (workspace-wide) ──"
VIOLATIONS=$(grep -rn 'allow(unsafe_code)' \
    */src/ \
    --include='*.rs' \
    2>/dev/null | \
    grep -v 'transport/bluetooth.rs' | \
    grep -v 'thd75-tui/src/main.rs' | \
    grep -v 'mmdvm_gateway_probe' || true)
if [ -n "$VIOLATIONS" ]; then
    echo "ERROR: allow(unsafe_code) found outside the allowlist:"
    echo "$VIOLATIONS"
    failed=1
fi

# Clippy: workspace-wide, every target. `--workspace --all-targets`
# iterates every crate in `[workspace] members`, including tests,
# examples, and benches.
run cargo clippy --workspace --all-targets -- -D warnings

# Feature-specific clippy: the `encoder` and `kenwood-tables` gates on
# mbelib-rs compile additional modules (encode/, encode/kenwood/)
# that default clippy wouldn't see. Run once per non-default feature
# set that the crate advertises.
run cargo clippy -p mbelib-rs --all-targets --features encoder -- -D warnings
run cargo clippy -p mbelib-rs --all-targets --features kenwood-tables -- -D warnings

# Tests: workspace-wide. `--workspace` walks every crate; the
# `--lib` form below runs only unit tests to skip integration
# fixtures that depend on gitignored paths (`docs/ki4lax_cat_spec.json`
# etc.). The full test set with fixtures runs in this script too
# because local checkouts have the fixtures.
run cargo test --workspace

# Feature-specific tests: mirror the clippy-feature matrix above.
# Default `encoder`-off covers the decoder surface; turning `encoder`
# on exposes `encode/` tests; `kenwood-tables` pulls in the
# integrity-test suite under `tests/kenwood_tables_integrity.rs`.
run cargo test -p mbelib-rs --features encoder
run cargo test -p mbelib-rs --features kenwood-tables

# Docs: workspace build with `-D warnings` so broken doc links (e.g.
# a `[`priv_fn`]` link from a pub item) hard-fail instead of printing
# a yellow warning.
RUSTDOCFLAGS="-D warnings" run cargo doc --workspace --no-deps

# Format: workspace-wide via `--all`.
run cargo fmt --all -- --check

# Security & quality checks — REQUIRED, not optional. A missing tool
# is a hard failure: partial gating is worse than no gating because it
# lets a silent regression land on push. If a tool is legitimately
# unavailable on the build host, install it explicitly rather than
# silently skipping the check.
for tool in cargo-audit cargo-deny cargo-machete; do
    if ! command -v "$tool" &>/dev/null; then
        echo "ERROR: $tool is required for the lint gate. Install with:"
        echo "  cargo install $tool"
        failed=1
    fi
done

# `cargo-audit` / `cargo-deny` both honour the workspace `Cargo.lock`
# at the repo root. `cargo-machete` spots unused `[dependencies]`
# entries across every workspace crate when pointed at `.`.
run cargo audit --file Cargo.lock
run cargo deny check
run cargo machete .

if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
