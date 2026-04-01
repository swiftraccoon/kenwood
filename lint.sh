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

run cargo clippy --manifest-path thd75/Cargo.toml -- -D warnings
run cargo clippy --manifest-path thd75-tui/Cargo.toml -- -D warnings
run cargo test --manifest-path thd75/Cargo.toml --lib
run cargo doc --manifest-path thd75/Cargo.toml --no-deps
run cargo fmt --manifest-path thd75/Cargo.toml -- --check
run cargo fmt --manifest-path thd75-tui/Cargo.toml -- --check

if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
