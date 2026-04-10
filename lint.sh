#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

failed=0

run() {
    echo "── $* ──"
    if ! "$@"; then
        failed=1
    fi
}

# Unsafe code audit: allow(unsafe_code) must only appear in bluetooth.rs
echo "── unsafe audit ──"
VIOLATIONS=$(grep -rn 'allow(unsafe_code)' thd75/src/ thd75-tui/src/ thd75-repl/src/ dstar-gateway/src/ --include='*.rs' | grep -v 'transport/bluetooth.rs' | grep -v 'thd75-tui/src/main.rs' | grep -v 'mmdvm_gateway_probe' || true)
if [ -n "$VIOLATIONS" ]; then
    echo "ERROR: allow(unsafe_code) found outside transport/bluetooth.rs:"
    echo "$VIOLATIONS"
    failed=1
fi

# Clippy (all crates)
run cargo clippy --manifest-path thd75/Cargo.toml --all-targets -- -D warnings
run cargo clippy --manifest-path thd75-tui/Cargo.toml --all-targets -- -D warnings
run cargo clippy --manifest-path dstar-gateway/Cargo.toml --all-targets -- -D warnings
run cargo clippy --manifest-path thd75-repl/Cargo.toml --all-targets -- -D warnings

# Tests (all crates)
run cargo test --manifest-path thd75/Cargo.toml
run cargo test --manifest-path dstar-gateway/Cargo.toml

# Docs (workspace)
RUSTDOCFLAGS="-D warnings" run cargo doc --workspace --no-deps

# Format (all crates)
run cargo fmt --manifest-path thd75/Cargo.toml -- --check
run cargo fmt --manifest-path thd75-tui/Cargo.toml -- --check
run cargo fmt --manifest-path dstar-gateway/Cargo.toml -- --check
run cargo fmt --manifest-path thd75-repl/Cargo.toml -- --check

# Security & quality checks (optional — only run if tools are installed)
if command -v cargo-audit &>/dev/null; then
    run cargo audit --file Cargo.lock
else
    echo "── cargo-audit not installed, skipping (cargo install cargo-audit) ──"
fi

if command -v cargo-deny &>/dev/null; then
    run cargo deny check
else
    echo "── cargo-deny not installed, skipping (cargo install cargo-deny) ──"
fi

if command -v cargo-machete &>/dev/null; then
    run cargo machete thd75/
else
    echo "── cargo-machete not installed, skipping (cargo install cargo-machete) ──"
fi

if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
