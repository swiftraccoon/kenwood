#!/usr/bin/env bash
# Run CI checks on local k8s cluster (Linux + Fedora) before pushing.
# Also runs macOS checks locally. Requires kubectl access to a cluster.
set -euo pipefail
cd "$(dirname "$0")"

failed=0
run() {
    echo "── $* ──"
    if ! "$@"; then failed=1; fi
}

# --- macOS (local) ---
echo "========== macOS (local) =========="
run ./lint.sh

# --- Create tarball ---
echo ""
echo "========== Preparing k8s CI =========="
tar czf /tmp/kenwood-ci.tar.gz \
    --exclude='*/target' --exclude='.git' --exclude='thd75_re' \
    --exclude='docs' --exclude='.superpowers' --exclude='.claude' . 2>/dev/null

ci_pod() {
    local name=$1 image=$2 setup=$3

    echo ""
    echo "========== $name =========="

    kubectl run "ci-$name" --image="$image" --command -- sleep 600 2>/dev/null
    kubectl wait --for=condition=Ready "pod/ci-$name" --timeout=120s 2>/dev/null

    kubectl exec "ci-$name" -- mkdir -p /work/kenwood 2>/dev/null
    kubectl cp /tmp/kenwood-ci.tar.gz "ci-$name:/work/kenwood.tar.gz" 2>/dev/null
    kubectl exec "ci-$name" -- bash -c "cd /work/kenwood && tar xzf /work/kenwood.tar.gz" 2>/dev/null

    # Install deps
    kubectl exec "ci-$name" -- bash -c "$setup" 2>/dev/null

    # Run checks
    kubectl exec "ci-$name" -- bash -c '
        cd /work/kenwood
        echo "--- check lib ---"
        cargo check --manifest-path thd75/Cargo.toml 2>&1 | tail -1
        echo "--- check tui ---"
        cargo check --manifest-path thd75-tui/Cargo.toml 2>&1 | tail -1
        echo "--- clippy lib ---"
        cargo clippy --manifest-path thd75/Cargo.toml -- -D warnings 2>&1 | tail -1
        echo "--- clippy tui ---"
        cargo clippy --manifest-path thd75-tui/Cargo.toml -- -D warnings 2>&1 | tail -1
        echo "--- test ---"
        cargo test --manifest-path thd75/Cargo.toml --lib 2>&1 | tail -1
        echo "--- doc ---"
        RUSTDOCFLAGS="-D warnings" cargo doc --manifest-path thd75/Cargo.toml --no-deps 2>&1 | tail -1
    ' 2>&1 || failed=1

    kubectl delete pod "ci-$name" --grace-period=0 --force 2>/dev/null &
}

ci_pod "ubuntu" "rust:1.89" "" &
UBUNTU_PID=$!

ci_pod "fedora" "fedora:latest" "dnf install -y gcc rust cargo clippy 2>/dev/null | tail -1" &
FEDORA_PID=$!

wait $UBUNTU_PID
wait $FEDORA_PID

# Cleanup tarball
rm -f /tmp/kenwood-ci.tar.gz

echo ""
if [ "$failed" -ne 0 ]; then
    echo "========== CI FAILED =========="
    exit 1
fi
echo "========== CI PASSED (macOS + Ubuntu + Fedora) =========="
