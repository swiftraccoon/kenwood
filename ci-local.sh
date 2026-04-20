#!/usr/bin/env bash
# Run CI checks on local k8s cluster (Linux + Fedora) before pushing.
# Also runs macOS checks locally. Requires kubectl access to a cluster.
#
# Pass-through policy: any tool or command the pod needs must be
# installed; a missing tool is a hard failure. The earlier
# "optional — skip if missing" pattern hid real regressions because
# the Docs workflow's alsa dependency could silently fall through.
set -euo pipefail
cd "$(dirname "$0")"

# Names of the pods this script creates. Keep them in sync with the
# `ci_pod "<name>"` calls below so the cleanup trap can find them.
CI_POD_NAMES=(ci-ubuntu ci-fedora)

# Delete any pods this script owns. Safe to call multiple times —
# `--ignore-not-found` treats "already gone" as success. Synchronous
# (no trailing `&`) so we actually wait for the API to accept the
# delete before the script exits.
cleanup_pods() {
    for n in "${CI_POD_NAMES[@]}"; do
        kubectl delete pod "$n" --grace-period=0 --force --ignore-not-found >/dev/null 2>&1 || true
    done
    rm -f /tmp/kenwood-ci.tar.gz
}

# EXIT covers normal finish AND `set -e` errors mid-run.
# INT / TERM cover Ctrl-C and external kills (harness timeouts,
# foreground SIGTERM, etc.) — without these the previous script
# version leaked pods whenever anything upstream killed it.
trap cleanup_pods EXIT INT TERM

failed=0
run() {
    echo "── $* ──"
    if ! "$@"; then failed=1; fi
}

# Pre-clean. If a prior run crashed before its trap fired (rare but
# possible), or if someone ran the bare `kubectl run` manually, stale
# pods would collide with this run's `kubectl run ci-ubuntu` etc.
# Deleting up-front makes the script idempotent rather than
# dependent on the cluster being quiescent.
cleanup_pods

# --- macOS (local) ---
echo "========== macOS (local) =========="
run ./lint.sh

# --- Create tarball ---
echo ""
echo "========== Preparing k8s CI =========="
tar czf /tmp/kenwood-ci.tar.gz \
    --exclude='*/target' --exclude='.git' . 2>/dev/null

ci_pod() {
    local name=$1 image=$2 setup=$3

    echo ""
    echo "========== $name =========="

    # Create and wait for the pod. The 2>/dev/null that used to wrap
    # these was swallowing real errors (e.g. a pre-existing pod
    # blocking `kubectl run`), leaving the script to press on against
    # stale or misconfigured pods. Let kubectl's error output through
    # so a failed setup is diagnosable from the first ci-local log.
    kubectl run "ci-$name" --image="$image" --command -- sleep 600
    kubectl wait --for=condition=Ready "pod/ci-$name" --timeout=120s

    kubectl exec "ci-$name" -- mkdir -p /work/kenwood
    kubectl cp /tmp/kenwood-ci.tar.gz "ci-$name:/work/kenwood.tar.gz"
    kubectl exec "ci-$name" -- bash -c "cd /work/kenwood && tar xzf /work/kenwood.tar.gz"

    # Install deps (image-specific bootstrap like rustup-on-fedora).
    kubectl exec "ci-$name" -- bash -c "$setup"

    # Run checks (source cargo env for rustup-installed toolchains).
    #
    # `set -eo pipefail` inside the pod script so any command failure
    # aborts and propagates a non-zero exit up through `kubectl exec`,
    # which flips our outer `failed=1`. Previously every command piped
    # through `| tail -1`, which hid cargo's exit code behind tail's
    # (always 0), so pod failures went unreported — the Docs workflow
    # broke on libasound2-dev missing for sextant/cpal without
    # ci-local flagging it, because the pod's `cargo doc --workspace`
    # silently failed.
    #
    # The `step` helper echoes a delimiter, runs the command with
    # `set -e` semantics, and keeps stderr on the normal output path
    # so context is visible without `tail -1` hiding cargo errors.
    kubectl exec "ci-$name" -- bash -c '
        set -eo pipefail
        [ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
        cd /work/kenwood
        step() { echo "--- $1 ---"; shift; "$@"; }

        # sextant (cpal) needs ALSA headers on Linux; install up-front
        # so the workspace-wide cargo commands see them. Missing ALSA
        # is exactly the silent regression the Docs workflow hit.
        if command -v apt-get >/dev/null 2>&1; then
            sudo apt-get update >/dev/null
            sudo apt-get install -y libasound2-dev >/dev/null
        elif command -v dnf >/dev/null 2>&1; then
            sudo dnf install -y alsa-lib-devel >/dev/null
        fi

        # Install required lint-gate tools. Fail hard if install
        # fails; dont skip-if-missing.
        cargo install cargo-audit --quiet
        cargo install cargo-deny --quiet
        cargo install cargo-machete --quiet

        # Workspace-wide checks: each command walks every crate in
        # `[workspace] members`, so new crates are picked up
        # automatically without editing this script.
        step "check workspace"       cargo check --workspace --all-targets
        step "clippy workspace"      cargo clippy --workspace --all-targets -- -D warnings
        step "clippy +encoder"       cargo clippy -p mbelib-rs --all-targets --features encoder -- -D warnings
        step "clippy +kenwood"       cargo clippy -p mbelib-rs --all-targets --features kenwood-tables -- -D warnings
        # Integration tests in thd75 depend on gitignored fixtures
        # (docs/ki4lax_cat_spec.json), so stick to --lib in CI.
        step "test workspace (lib)"  cargo test --workspace --lib
        step "test +encoder"         cargo test -p mbelib-rs --features encoder --lib
        step "test +kenwood"         cargo test -p mbelib-rs --features kenwood-tables --lib
        step "doc workspace"         env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
        step "audit"                 cargo audit --file Cargo.lock
        step "deny"                  cargo deny check
        step "machete"               cargo machete .
    ' 2>&1 || failed=1

    # Per-pod delete intentionally removed: the EXIT/INT/TERM trap at
    # the top of this script calls `cleanup_pods` which deletes every
    # pod name in `CI_POD_NAMES`. A backgrounded `kubectl delete` at
    # this point (the previous design) would fire-and-forget — the
    # script was free to exit before the API server processed the
    # request, leaving the pods running for the 22-hour+ lifetimes
    # we observed in practice.
}

ci_pod "ubuntu" "rust:1.94" "" &
UBUNTU_PID=$!

ci_pod "fedora" "fedora:latest" "dnf install -y gcc 2>/dev/null | tail -1 && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.94.0 -c clippy 2>/dev/null | tail -1" &
FEDORA_PID=$!

wait $UBUNTU_PID
wait $FEDORA_PID

# Cleanup runs via the EXIT trap registered at the top. No manual
# tarball / pod removal here; the trap handles both synchronously.

echo ""
if [ "$failed" -ne 0 ]; then
    echo "========== CI FAILED =========="
    exit 1
fi
echo "========== CI PASSED (macOS + Ubuntu + Fedora) =========="
