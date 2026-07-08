#!/usr/bin/env bash
# Full-workspace gate. Use workspace-level cargo invocations wherever
# possible so new crates added under `[workspace] members` are picked
# up automatically without editing this file. `-p <crate>` narrows the
# heavy cargo steps to one package for the edit loop; the full gate is
# still the pre-commit bar.
set -euo pipefail

cd "$(dirname "$0")"

# ---------- args ----------
QUIET=0
FIX=0
PKG=""
while [ $# -gt 0 ]; do
    case "$1" in
        -q|--quiet) QUIET=1 ;;
        --fix)      FIX=1 ;;
        -p|--package)
            if [ $# -lt 2 ]; then
                echo "Missing crate name after $1" >&2
                echo "Try '$(basename "$0") --help'" >&2
                exit 2
            fi
            PKG="$2"
            shift
            ;;
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

  -p, --package CRATE
                 Narrow the slow cargo steps (clippy, test, doc)
                 to one package for a fast edit loop. Use the
                 PACKAGE name (kenwood-thd75, not thd75). The
                 mbelib-rs feature-matrix steps run only when the
                 target is mbelib-rs. Cheap workspace-wide steps
                 (unsafe audit, fmt, audit/deny/machete, shellcheck,
                 taplo) always run. A scoped pass does NOT lint the
                 crates that depend on the target — run the full
                 gate before committing.

  --fix          Auto-apply mechanical fixes BEFORE running the
                 gate: 'cargo fmt --all' and
                 'cargo clippy --fix --allow-dirty' across the
                 workspace (or the -p target) and the mbelib-rs
                 feature matrix. The full gate then runs unchanged
                 so you can confirm the edits produced a clean
                 build.

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
            echo "Unknown argument: $1" >&2
            echo "Try '$(basename "$0") --help'" >&2
            exit 2
            ;;
    esac
    shift
done

# Cargo scope for the slow steps: the whole workspace by default,
# a single package when -p was given.
if [ -n "$PKG" ]; then
    SCOPE=(-p "$PKG")
else
    SCOPE=(--workspace)
fi

# The mbelib-rs feature matrix only adds signal when mbelib-rs is in
# scope — skip it when the gate is narrowed to some other crate.
MBELIB_MATRIX=1
if [ -n "$PKG" ] && [ "$PKG" != "mbelib-rs" ]; then
    MBELIB_MATRIX=0
fi

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
    run cargo clippy --fix --allow-dirty "${SCOPE[@]}" --all-targets
    if [ "$MBELIB_MATRIX" -eq 1 ]; then
        run cargo clippy --fix --allow-dirty -p mbelib-rs --all-targets --features encoder
        run cargo clippy --fix --allow-dirty -p mbelib-rs --all-targets --features kenwood-tables
    fi
fi

# ---------- unsafe audit ----------

# Two-layer source-tree check. Cargo enforces `unsafe_code = "forbid"`
# for every `[lints] workspace = true` crate, but the override crates
# (thd75, thd75-tui, lodestar-core) weaken or omit that lint, and
# thd75's source-level `#![deny(unsafe_code)]` guards only its lib
# target — probes/examples/tests are separate compilation units. This
# audit closes those gaps by scanning the tree directly:
#
# 1. `unsafe_code` suppression markers — `allow(unsafe_code)` or
#    `expect(unsafe_code)` in any form, including the multi-line
#    attribute style where `unsafe_code,` sits on its own line.
# 2. The `unsafe` keyword itself (blocks, fns, extern, attributes).
#    Comment-only lines are excluded; a mention inside a string still
#    trips the audit — a loud false positive beats a silent miss.
#
# Allowlist (each entry is a deliberate design decision):
#   thd75/src/transport/bluetooth.rs — IOBluetooth RFCOMM FFI
#   thd75-tui/src/main.rs            — CFRunLoop pump for IOBluetooth
check_unsafe_audit() {
    local allowlist='thd75/src/transport/bluetooth\.rs|thd75-tui/src/main\.rs'
    local suppressions keyword
    suppressions=$(grep -rnE \
        '(allow|expect)[[:space:]]*\([[:space:]]*unsafe_code|^[[:space:]]*unsafe_code[[:space:]]*,?[[:space:]]*$' \
        ./*/src/ ./*/probes/ ./*/examples/ ./*/tests/ \
        --include='*.rs' \
        2>/dev/null | \
        grep -vE "$allowlist" || true)
    keyword=$(grep -rnE '(^|[^A-Za-z0-9_])unsafe($|[^A-Za-z0-9_])' \
        ./*/src/ ./*/probes/ ./*/examples/ ./*/tests/ \
        --include='*.rs' \
        2>/dev/null | \
        grep -vE "$allowlist" | \
        grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|\*)' || true)
    if [ -n "$suppressions" ] || [ -n "$keyword" ]; then
        echo "ERROR: unsafe usage or unsafe_code suppression outside the allowlist:"
        if [ -n "$suppressions" ]; then echo "$suppressions"; fi
        if [ -n "$keyword" ]; then echo "$keyword"; fi
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
    for tool in cargo-audit cargo-deny cargo-machete shellcheck taplo; do
        if ! command -v "$tool" &>/dev/null; then
            missing+=("$tool")
        fi
    done
    if [ ${#missing[@]} -gt 0 ]; then
        echo "ERROR: required lint-gate tools are missing: ${missing[*]}"
        echo "Install with:"
        for tool in "${missing[@]}"; do
            case "$tool" in
                shellcheck) echo "  brew install shellcheck   # or: apt install shellcheck" ;;
                taplo)      echo "  cargo install taplo-cli --locked" ;;
                *)          echo "  cargo install $tool" ;;
            esac
        done
        return 1
    fi
    return 0
}

run_inline "required tools present" check_required_tools

# ---------- cargo gates ----------

# Clippy: every target in scope. `--all-targets` covers tests,
# examples, and benches; the scope is the workspace by default or a
# single `-p` package for the edit loop.
run cargo clippy "${SCOPE[@]}" --all-targets -- -D warnings

# Feature-specific clippy: the `encoder` and `kenwood-tables` gates on
# mbelib-rs compile additional modules (encode/, encode/kenwood/)
# that default clippy wouldn't see. Run once per non-default feature
# set that the crate advertises.
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run cargo clippy -p mbelib-rs --all-targets --features encoder -- -D warnings
    run cargo clippy -p mbelib-rs --all-targets --features kenwood-tables -- -D warnings
fi

# Tests: same scope as clippy. Local checkouts have the
# integration-test fixtures (the ci/docs-workflow variant restricts
# to `--lib`). Default + feature matrix mirrors the clippy matrix so
# feature-gated tests are exercised.
run cargo test "${SCOPE[@]}"
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run cargo test -p mbelib-rs --features encoder
    run cargo test -p mbelib-rs --features kenwood-tables
fi

# Docs: scoped build with `-D warnings` so broken doc links (e.g.
# a `[`priv_fn`]` link from a pub item) hard-fail instead of printing
# a yellow warning.
RUSTDOCFLAGS="-D warnings" run cargo doc "${SCOPE[@]}" --no-deps

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

# Static-analyze the lint gate scripts themselves. Both files contain
# trap handlers, kubectl heredocs, and pipefail-sensitive pipelines;
# running shellcheck catches the bash foot-guns those introduce.
run shellcheck lint.sh ci-local.sh

# Verify every workspace `Cargo.toml` is taplo-formatted. Catches manifest
# drift (trailing whitespace, key reordering) before it reaches review.
run taplo fmt --check Cargo.toml '*/Cargo.toml'

# ---------- summary ----------
echo
if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
