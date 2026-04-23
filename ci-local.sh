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
# echo "========== macOS (local) =========="
# run ./lint.sh

# --- Create tarball ---
echo ""
echo "========== Preparing k8s CI =========="
tar czf /tmp/kenwood-ci.tar.gz \
    --exclude='target' --exclude='.git' \
    --exclude='ref' --exclude='ref_tools' \
    --exclude='thd75_re' . 2>/dev/null

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
    # Run checks. `kubectl exec` returns the exit code of the remote
    # command, so letting the exit code propagate up makes this the
    # last command in the function — and the subshell spawned by
    # `ci_pod &` inherits that exit status. The parent's `wait` then
    # surfaces the failure via its own exit code, which drives
    # `failed=1` back in the main shell. An earlier version wrapped
    # this with `|| failed=1`, but since ci_pod runs in a subshell
    # the assignment was local-only and every pod failure was
    # silently swallowed (the script always printed "CI PASSED").
    kubectl exec "ci-$name" -- bash -c '
        set -eo pipefail
        [ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
        cd /work/kenwood
        step() { echo "--- $1 ---"; shift; "$@"; }

        # Pod-level system deps. Neither base image ships these out
        # of the box:
        #   libasound2-dev / alsa-lib-devel — sextant (cpal) needs
        #     ALSA headers. Missing ALSA is exactly the silent
        #     regression the Docs workflow hit previously.
        #   git — cargo-deny clones the RustSec advisory database
        #     via `git clone`, and neither `rust:1.94` nor
        #     `fedora:latest` preinstalls a git binary. Without
        #     this, `cargo deny check` fails with
        #     "failed to spawn git: No such file or directory".
        # No `sudo` — both base images run as root and neither
        # ships sudo; adding it would break with "command not
        # found".
        if command -v apt-get >/dev/null 2>&1; then
            apt-get update >/dev/null
            apt-get install -y libasound2-dev git >/dev/null
        elif command -v dnf >/dev/null 2>&1; then
            dnf install -y alsa-lib-devel git >/dev/null
        fi

        # Ensure clippy is present. Neither `rust:1.94` nor the
        # `rustup --profile minimal` install on fedora ships clippy
        # by default — a previous run surfaced the mistake only at
        # the `cargo clippy` step, far too late. rustup component
        # add is idempotent, so running it unconditionally is cheap
        # on pods that already have it.
        rustup component add clippy >/dev/null

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
        # Some thd75 integration tests depend on gitignored spec
        # fixtures that aren't shipped in CI, so stick to --lib here.
        step "test workspace (lib)"  cargo test --workspace --lib
        step "test +encoder"         cargo test -p mbelib-rs --features encoder --lib
        step "test +kenwood"         cargo test -p mbelib-rs --features kenwood-tables --lib
        step "doc workspace"         env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
        step "audit"                 cargo audit --file Cargo.lock
        step "deny"                  cargo deny check
        step "machete"               cargo machete .
    ' 2>&1

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

ci_pod "fedora" "fedora:latest" "
    set -eo pipefail
    # Fedora base image ships neither a compiler nor rustup. Install
    # gcc first (rustc's linker needs it), then rustup-init with an
    # explicit minimal profile. Clippy is added by the inner lint
    # script via 'rustup component add clippy' — kept there as a
    # single source of truth that handles both pods. Dropped the
    # ' | tail -1' mask that used to hide install failures (see
    # the 'pass-through policy' comment at the top of this file).
    #
    # Retry loop on 'dnf install -y gcc': fedora:latest sometimes
    # hits transient 'package X does not verify: (null)' RPM
    # checksum/signature failures when fc43 just-released packages
    # haven't fully propagated to all mirrors. 'dnf clean metadata'
    # between retries forces a fresh download of the repodata,
    # which often picks up a different mirror.
    n=0
    until dnf install -y gcc; do
        n=\$((n + 1))
        [ \"\$n\" -ge 3 ] && { echo 'dnf install failed after 3 attempts'; exit 1; }
        echo \"dnf retry \$n of 3\"
        dnf clean metadata
        sleep 5
    done
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.94.0
    source \$HOME/.cargo/env
" &
FEDORA_PID=$!

# `|| failed=1` here (in the PARENT shell, not the subshell) is the
# actual gate that sets the overall CI status. `wait $PID` reports
# the exit code of the backgrounded ci_pod invocation; without the
# `||` the shell's `set -e` would abort before the second pod is
# awaited, and without assigning to `failed` the final summary
# line would lie. Both pods are awaited unconditionally so we get
# a complete status report even when one fails.
wait "$UBUNTU_PID" || failed=1
wait "$FEDORA_PID" || failed=1

# Cleanup runs via the EXIT trap registered at the top. No manual
# tarball / pod removal here; the trap handles both synchronously.

echo ""
if [ "$failed" -ne 0 ]; then
    echo "========== CI FAILED =========="
    exit 1
fi
echo "========== CI PASSED (macOS + Ubuntu + Fedora) =========="
