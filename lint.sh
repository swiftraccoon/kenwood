#!/usr/bin/env bash
set -euo pipefail

echo "=== thd75 library ==="
(cd thd75 && cargo clippy -- -D warnings)
(cd thd75 && cargo test --lib)

echo "=== thd75-tui ==="
(cd thd75-tui && cargo clippy -- -D warnings)

echo "=== all clean ==="
