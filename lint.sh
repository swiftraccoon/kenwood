#!/usr/bin/env bash
# Full-workspace gate. Use workspace-level cargo invocations wherever
# possible so new crates added under `[workspace] members` are picked
# up automatically without editing this file.
set -euo pipefail

cd "$(dirname "$0")"

# ---------- args ----------
QUIET=0
FIX=0
for arg in "$@"; do
    case "$arg" in
        -q|--quiet) QUIET=1 ;;
        --fix)      FIX=1 ;;
        -h|--help)
            cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Runs the full-workspace lint gate: unsafe audit, clippy (incl.
mbelib-rs feature matrix), tests, docs, fmt, cargo-audit,
cargo-deny, cargo-machete.

Options:
  -q, --quiet    Suppress output from passing checks. Each step
                 prints a single "✓ step (<elapsed>s)" line on
                 success and the full captured stdout+stderr plus
                 "✗ step (<elapsed>s)" on failure. The final
                 OK/FAILED summary still prints.

  --fix          Auto-apply mechanical fixes BEFORE running the
                 gate: 'cargo fmt --all' and
                 'cargo clippy --fix --allow-dirty' across the
                 workspace and the mbelib-rs feature matrix.
                 The full gate then runs unchanged so you can
                 confirm the edits produced a clean build.

  -h, --help     Show this help.

On any failing step, the captured output is saved to
  .lint-failures/<sanitised-step>.log
so you can paste it into a bug report without re-running. The
directory is created on demand and is matched by the repo-wide
'*.log' gitignore rule.

Exit code is 0 if every step passes, 1 otherwise.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            echo "Try '$(basename "$0") --help'" >&2
            exit 2
            ;;
    esac
done

# ---------- failure log preservation ----------
FAIL_DIR=.lint-failures

# Derive a filesystem-safe name from a step description. Spaces and
# slashes become underscores; anything outside [A-Za-z0-9._-] is
# collapsed into '_' so the result is shell- and path-safe without
# quoting. Same description always yields the same filename, so a
# re-run overwrites rather than accumulating `<step>.log.1`,
# `<step>.log.2`, etc.
sanitise() {
    printf '%s' "$1" | sed 's/[^a-zA-Z0-9._-]/_/g'
}

save_failure_log() {
    local desc="$1"
    local src="$2"
    local name
    name=$(sanitise "$desc")
    mkdir -p "$FAIL_DIR"
    # mv rather than cp — the temp is ours, moving is atomic and
    # avoids a second disk write.
    mv "$src" "$FAIL_DIR/${name}.log"
    printf '  (output saved to %s/%s.log)\n' "$FAIL_DIR" "$name"
}

# ---------- step runners ----------
failed=0

# Run a single command as a lint step. Always captures combined
# output so that on failure we can both display it and persist it
# to `.lint-failures/`. In quiet mode we only dump the buffer on
# failure; in the default (streaming) mode we tee it to the
# terminal live and save the same bytes on failure.
#
# `tee` disables cargo's ANSI colours (cargo detects that stdout
# is a pipe). The trade-off: consistent log files, monochrome
# terminal. Set `CARGO_TERM_COLOR=always` in your env if you want
# colours back (the logs will then contain ANSI codes too).
run() {
    local desc="$*"
    local start=$SECONDS
    local tmp
    tmp=$(mktemp)
    local ok
    if [ "$QUIET" -eq 1 ]; then
        if "$@" >"$tmp" 2>&1; then ok=1; else ok=0; fi
    else
        echo "── $desc ──"
        # `pipefail` (set at the top) makes the pipeline exit
        # with the command's exit code even though tee itself
        # succeeded, so `ok` reflects the real result.
        if "$@" 2>&1 | tee "$tmp"; then ok=1; else ok=0; fi
    fi
    local elapsed=$((SECONDS - start))
    if [ "$ok" -eq 1 ]; then
        if [ "$QUIET" -eq 1 ]; then
            printf '✓ %s  (%ds)\n' "$desc" "$elapsed"
        fi
        rm -f "$tmp"
    else
        if [ "$QUIET" -eq 1 ]; then
            printf '✗ %s  (%ds)\n' "$desc" "$elapsed"
            cat "$tmp"
        else
            printf '✗ %s  (%ds)\n' "$desc" "$elapsed"
        fi
        save_failure_log "$desc" "$tmp"
        failed=1
    fi
}

# Variant of `run` for steps whose body is an inline shell function
# rather than a single command. Same capture and persistence rules.
run_inline() {
    local desc="$1"
    local fn="$2"
    local start=$SECONDS
    local tmp
    tmp=$(mktemp)
    local ok
    if [ "$QUIET" -eq 1 ]; then
        if "$fn" >"$tmp" 2>&1; then ok=1; else ok=0; fi
    else
        echo "── $desc ──"
        if "$fn" 2>&1 | tee "$tmp"; then ok=1; else ok=0; fi
    fi
    local elapsed=$((SECONDS - start))
    if [ "$ok" -eq 1 ]; then
        if [ "$QUIET" -eq 1 ]; then
            printf '✓ %s  (%ds)\n' "$desc" "$elapsed"
        fi
        rm -f "$tmp"
    else
        if [ "$QUIET" -eq 1 ]; then
            printf '✗ %s  (%ds)\n' "$desc" "$elapsed"
            cat "$tmp"
        else
            printf '✗ %s  (%ds)\n' "$desc" "$elapsed"
        fi
        save_failure_log "$desc" "$tmp"
        failed=1
    fi
}

# ---------- optional auto-fix pass ----------
# Runs BEFORE the gate so the gate sees the fixed tree. Failures
# here still fail the overall run via the normal `failed` path, so
# e.g. an internal clippy panic during --fix doesn't silently pass.
# Clippy's --fix is given --allow-dirty so it doesn't refuse to
# run on a repo with uncommitted changes; the expected workflow is
# "edit, lint.sh --fix, review diff, commit".
if [ "$FIX" -eq 1 ]; then
    run cargo fmt --all
    run cargo clippy --fix --allow-dirty --workspace --all-targets
    run cargo clippy --fix --allow-dirty -p mbelib-rs --all-targets --features encoder
    run cargo clippy --fix --allow-dirty -p mbelib-rs --all-targets --features kenwood-tables
fi

# ---------- unsafe audit ----------

# `allow(unsafe_code)` must appear ONLY in the known-required spots.
# Globs cover every crate's `src/`; add new exclusions as deliberate
# design decisions, never blanket-whitelist.
check_unsafe_audit() {
    local violations
    violations=$(grep -rn 'allow(unsafe_code)' \
        */src/ \
        --include='*.rs' \
        2>/dev/null | \
        grep -v 'transport/bluetooth.rs' | \
        grep -v 'thd75-tui/src/main.rs' | \
        grep -v 'mmdvm_gateway_probe' || true)
    if [ -n "$violations" ]; then
        echo "ERROR: allow(unsafe_code) found outside the allowlist:"
        echo "$violations"
        return 1
    fi
    return 0
}

run_inline "unsafe audit (workspace-wide)" check_unsafe_audit

# ---------- required external tools ----------
# A missing tool is a hard failure; partial gating is worse than no
# gating because a silent regression could land on push.
check_required_tools() {
    local missing=()
    local tool
    for tool in cargo-audit cargo-deny cargo-machete; do
        if ! command -v "$tool" &>/dev/null; then
            missing+=("$tool")
        fi
    done
    if [ ${#missing[@]} -gt 0 ]; then
        echo "ERROR: required lint-gate tools are missing: ${missing[*]}"
        echo "Install with:"
        for tool in "${missing[@]}"; do
            echo "  cargo install $tool"
        done
        return 1
    fi
    return 0
}

run_inline "required tools present" check_required_tools

# ---------- cargo gates ----------

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

# Tests: workspace-wide. `--workspace` walks every crate; local
# checkouts have the integration-test fixtures (the ci/docs-workflow
# variant restricts to `--lib`). Default + feature matrix mirrors
# the clippy matrix so feature-gated tests are exercised.
run cargo test --workspace
run cargo test -p mbelib-rs --features encoder
run cargo test -p mbelib-rs --features kenwood-tables

# Docs: workspace build with `-D warnings` so broken doc links (e.g.
# a `[`priv_fn`]` link from a pub item) hard-fail instead of printing
# a yellow warning.
RUSTDOCFLAGS="-D warnings" run cargo doc --workspace --no-deps

# Format: workspace-wide via `--all`. When `--fix` was passed we
# already ran `cargo fmt --all` in apply mode above, so this is the
# verification pass.
run cargo fmt --all -- --check

# `cargo-audit` / `cargo-deny` both honour the workspace `Cargo.lock`
# at the repo root. `cargo-machete` spots unused `[dependencies]`
# entries across every workspace crate when pointed at `.`.
run cargo audit --file Cargo.lock
run cargo deny check
run cargo machete .

# ---------- summary ----------
echo
if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
