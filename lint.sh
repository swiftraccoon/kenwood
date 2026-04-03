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
VIOLATIONS=$(grep -rn 'allow(unsafe_code)' thd75/src/ thd75-tui/src/ --include='*.rs' | grep -v 'transport/bluetooth.rs' | grep -v 'thd75-tui/src/main.rs' || true)
if [ -n "$VIOLATIONS" ]; then
    echo "ERROR: allow(unsafe_code) found outside transport/bluetooth.rs:"
    echo "$VIOLATIONS"
    failed=1
fi

run cargo clippy --manifest-path thd75/Cargo.toml -- -D warnings
run cargo clippy --manifest-path thd75-tui/Cargo.toml -- -D warnings
run cargo test --manifest-path thd75/Cargo.toml
RUSTDOCFLAGS="-D warnings" run cargo doc --manifest-path thd75/Cargo.toml --no-deps
run cargo fmt --manifest-path thd75/Cargo.toml -- --check
run cargo fmt --manifest-path thd75-tui/Cargo.toml -- --check

if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
