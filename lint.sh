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

Runs the full-project lint gate: unsafe audit, clippy (including
the mbelib-rs feature matrix and standalone fuzz workspace), tests
(cargo-nextest for executable tests, cargo test --doc for doctests,
the safe dstar-gateway feature matrix, and native macOS app tests),
docs, fmt, cargo-audit, cargo-deny,
cargo-machete, and Apple app builds on macOS. Steps that do not
contend for the same build cache run in parallel. The base executable
tests, native Azimuth/Lodestar lanes, and static checks overlap Clippy;
the isolated rustdoc lanes overlap the small feature-test matrix, and
full-workspace doctests run in stable crate shards on POSIX hosts.
Parallel output is buffered and reported per step.

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
                 crates that depend on the target; run the full
                 gate before committing.

  --fix          Auto-apply mechanical fixes BEFORE running the
                 gate: 'cargo fmt --all' and
                 'cargo clippy --fix --allow-dirty' across the
                 workspace (or the -p target) and the mbelib-rs
                 feature matrix. The full gate then runs unchanged
                 so you can confirm the edits produced a clean
                 build.

  -h, --help     Show this help.

Environment:
  LINT_JOBS=N
                 CPU budget for the Rust compile phases (default:
                 logical CPU count).

  LINT_DOCTEST_JOBS=N
                 Full-workspace doctest shards, from 1 to 8
                 (default: 4, capped by LINT_JOBS). Each shard
                 keeps a persistent cache under
                 target/doctest-gate/.

  LINT_XCODE_JOBS=N
                 Jobs for each parallel Azimuth/Lodestar Xcode lane
                 on macOS (default: 2). Each lane uses persistent
                 DerivedData under target/xcode-derived/.

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

HOST_KERNEL=$(uname -s 2>/dev/null || true)

# The fuzz targets intentionally form their own Cargo workspace. Include
# them in the full gate and when dstar-gateway-core is selected directly.
FUZZ_GATE=0
if [ -z "$PKG" ] || [ "$PKG" = "dstar-gateway-core" ]; then
    FUZZ_GATE=1
fi

# Native applications can only be built by Xcode on macOS. A full Darwin
# gate covers both apps; a scoped core pass covers its corresponding app.
AZIMUTH_APP_GATE=0
LODESTAR_APP_GATE=0
if [ "$HOST_KERNEL" = "Darwin" ]; then
    if [ -z "$PKG" ] || [ "$PKG" = "azimuth-core" ]; then
        AZIMUTH_APP_GATE=1
    fi
    if [ -z "$PKG" ] || [ "$PKG" = "lodestar-core" ]; then
        LODESTAR_APP_GATE=1
    fi
fi

# The mbelib-rs feature matrix only adds signal when mbelib-rs is in
# scope; skip it when the gate is narrowed to some other crate.
MBELIB_MATRIX=1
if [ -n "$PKG" ] && [ "$PKG" != "mbelib-rs" ]; then
    MBELIB_MATRIX=0
fi

# The mdBook lives under dstar-gateway; only build it when that crate
# is in scope.
BOOK_BUILD=1
if [ -n "$PKG" ] && [ "$PKG" != "dstar-gateway" ]; then
    BOOK_BUILD=0
fi

# The blocking adapter has tests and a doctest that no default workspace
# member enables. Exercise it explicitly without enabling the live-network
# hardware-tests or examples-network features.
DSTAR_BLOCKING_MATRIX=1
if [ -n "$PKG" ] && [ "$PKG" != "dstar-gateway" ]; then
    DSTAR_BLOCKING_MATRIX=0
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
    # mv rather than cp: the temp is ours, moving is atomic and
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
#
# Keep Clippy's compiler-mode fingerprints out of the normal Cargo
# target directory. Otherwise switching from `cargo clippy` to nextest
# relinks every test executable, and macOS reassesses all of them with
# Gatekeeper on every lint run.
CLIPPY_TARGET_DIR=target/clippy-gate
TEST_TARGET_DIR=target/test-gate
CLIPPY_ENV=(env CARGO_PROFILE_DEV_DEBUG=0)
TEST_ENV=(env CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0)
if [ "$FIX" -eq 1 ]; then
    run cargo fmt --all
    run cargo clippy --fix --allow-dirty "${SCOPE[@]}" --all-targets \
        --target-dir "$CLIPPY_TARGET_DIR"
    run cargo clippy --fix --allow-dirty "${SCOPE[@]}" --all-targets --all-features \
        --target-dir "$CLIPPY_TARGET_DIR"
    if [ "$MBELIB_MATRIX" -eq 1 ]; then
        run cargo clippy --fix --allow-dirty -p mbelib-rs --all-targets \
            --features encoder --target-dir "$CLIPPY_TARGET_DIR"
        run cargo clippy --fix --allow-dirty -p mbelib-rs --all-targets \
            --features kenwood-tables --target-dir "$CLIPPY_TARGET_DIR"
    fi
    if [ "$FUZZ_GATE" -eq 1 ]; then
        run cargo fmt --manifest-path dstar-gateway-core/fuzz/Cargo.toml --all
        run cargo clippy --fix --allow-dirty \
            --manifest-path dstar-gateway-core/fuzz/Cargo.toml --all-targets
    fi
fi

# ---------- unsafe audit ----------

# Workspace lint policy forbids unsafe code in ordinary crates. The TH-D75
# library has two reasoned FFI expectations; its package-level deny applies
# to every Cargo library, example, and test target. The two UniFFI crates
# cannot deny unsafe at the crate root because generated scaffolding contains
# the required C ABI. Their hand-written modules carry
# `#[forbid(unsafe_code)]`; this source check guards both hand-written source
# trees and the manual probe files, and prevents any new lint suppression
# outside the two audited TH-D75 FFI modules.
check_unsafe_audit() {
    local allowlist='thd75/src/transport/bluetooth\.rs|thd75/src/screen/vision\.rs'
    local suppressions keyword
    suppressions=$(grep -rnE \
        '(allow|expect)[[:space:]]*\([[:space:]]*unsafe_code|^[[:space:]]*unsafe_code[[:space:]]*,?[[:space:]]*$' \
        ./*/src/ ./*/probes/ ./*/examples/ ./*/tests/ \
        ./*/fuzz/fuzz_targets/ ./*/build.rs \
        --include='*.rs' \
        2>/dev/null | \
        grep -vE "$allowlist" || true)
    keyword=$(grep -rnE \
        'unsafe[[:space:]]*(extern|fn|trait|impl|\{)|#\[[[:space:]]*unsafe[[:space:]]*\(' \
        azimuth-core/src/ lodestar-core/src/ thd75/probes/ \
        --include='*.rs' \
        2>/dev/null | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|\*)' || true)
    if [ -n "$suppressions" ] || [ -n "$keyword" ]; then
        echo "ERROR: unsafe syntax or unsafe_code suppression outside the allowlist:"
        if [ -n "$suppressions" ]; then echo "$suppressions"; fi
        if [ -n "$keyword" ]; then echo "$keyword"; fi
        return 1
    fi
    return 0
}

run_inline "unsafe audit (workspace-wide)" check_unsafe_audit

# ---------- em-dash ban ----------
# House style: no em dashes (U+2014) in committed prose, meaning doc
# comments, code comments, READMEs, markdown, shell/TOML/YAML comments,
# and user-facing string literals. Punctuation is chosen per sentence
# (semicolon between independent clauses, colon before elaboration,
# comma pair or parentheses for an aside, or a reword), so a mechanical
# hyphen swap is never the fix. En dash ranges and box-drawing
# separators are unaffected: only U+2014 is banned.
#
# The pattern is assembled from its UTF-8 bytes rather than written
# literally, so this file does not trip its own check.
#
# TRACKED files only: in-flight untracked scratch must not redden the
# gate. Outside a git checkout (the CI pods receive a tarball) the list
# is empty and the step passes vacuously.
#
# Exempt, because their bytes are a contract rather than prose:
#   thd75/data/mcp_d75_menu_schema.json  generated menu manifest
#   thd75/src/memory/menu_fields.rs      generated field registry
#   mcp-d75-extract/tests/fixtures/      pinned decompilation fixtures
check_em_dashes() {
    local exempt='^thd75/data/mcp_d75_menu_schema\.json$|^thd75/src/memory/menu_fields\.rs$|^mcp-d75-extract/tests/fixtures/'
    local em files hits
    em=$(printf '\342\200\224')
    files=$(git ls-files 2>/dev/null | grep -vE "$exempt" || true)
    [ -n "$files" ] || return 0
    hits=$(printf '%s\n' "$files" | tr '\n' '\0' | \
        xargs -0 grep -HnIF -- "$em" 2>/dev/null || true)
    if [ -n "$hits" ]; then
        echo "ERROR: em dash (U+2014) in committed prose."
        echo "Use a semicolon between independent clauses, a colon before an"
        echo "elaboration, commas or parentheses for an aside, or reword."
        echo "$hits"
        return 1
    fi
    return 0
}

run_inline "em-dash ban (tracked files)" check_em_dashes

# ---------- required external tools ----------
# A missing tool is a hard failure; partial gating is worse than no
# gating because a silent regression could land on push.
check_required_tools() {
    local missing=()
    local tool
    for tool in cargo-audit cargo-deny cargo-machete cargo-nextest shellcheck taplo mdbook; do
        if ! command -v "$tool" &>/dev/null; then
            missing+=("$tool")
        fi
    done
    if [ "$AZIMUTH_APP_GATE" -eq 1 ] || [ "$LODESTAR_APP_GATE" -eq 1 ]; then
        for tool in xcodebuild xcodegen; do
            if ! command -v "$tool" &>/dev/null; then
                missing+=("$tool")
            fi
        done
    fi
    if [ ${#missing[@]} -gt 0 ]; then
        echo "ERROR: required lint-gate tools are missing: ${missing[*]}"
        echo "Install with:"
        for tool in "${missing[@]}"; do
            case "$tool" in
                shellcheck)    echo "  brew install shellcheck   # or: apt install shellcheck" ;;
                taplo)         echo "  cargo install taplo-cli --locked" ;;
                mdbook)        echo "  cargo install mdbook --locked" ;;
                cargo-nextest) echo "  cargo install cargo-nextest --locked" ;;
                xcodegen)      echo "  brew install xcodegen" ;;
                xcodebuild)    echo "  Install Xcode and select it with xcode-select" ;;
                *)             echo "  cargo install $tool" ;;
            esac
        done
        return 1
    fi
    return 0
}

run_inline "required tools present" check_required_tools

# Cargo.lock is intentionally ignored, but the security gates and the
# standalone fuzz workspace both require one. Materialize missing lockfiles,
# then resolve each graph before any parallel reader starts. This prevents a
# later Cargo build from updating a stale lockfile while audit or deny is
# concurrently inspecting the old graph.
ensure_lockfiles() {
    if [ ! -f Cargo.lock ]; then
        cargo generate-lockfile
    fi
    if [ "$FUZZ_GATE" -eq 1 ] && \
        [ ! -f dstar-gateway-core/fuzz/Cargo.lock ]; then
        cargo generate-lockfile \
            --manifest-path dstar-gateway-core/fuzz/Cargo.toml
    fi

    cargo metadata --format-version 1 --no-deps --all-features >/dev/null
    if [ "$FUZZ_GATE" -eq 1 ]; then
        cargo metadata \
            --manifest-path dstar-gateway-core/fuzz/Cargo.toml \
            --format-version 1 --no-deps >/dev/null
    fi
}

run_inline "Cargo lockfiles present" ensure_lockfiles

# ---------- parallel job groups ----------
# Independent checks are separate jobs, not one background subshell
# containing a serial command list. Output remains buffered per step so
# parallel work never interleaves terminal output.
#
# Five groups use this runner:
#   static   fmt/audit/deny/machete/shellcheck/taplo/mdbook/fuzz
#   docs     the two isolated rustdoc feature lanes
#   test     the base cargo-nextest workspace lane
#   doctest  stable crate shards, each with its own target directory
#   native   Azimuth and Lodestar build/test pipelines on macOS
#
# Every job writes four files keyed by a monotonically increasing id:
# .desc, .log, .status, and .time. Files carry child state back to the
# parent without relying on subshell variable assignments.
BG_DIR=$(mktemp -d)
BG_NEXT_ID=0
STATIC_IDS=()
STATIC_PIDS=()
STATIC_ACTIVE=0
DOC_IDS=()
DOC_PIDS=()
DOC_ACTIVE=0
TEST_IDS=()
TEST_PIDS=()
TEST_ACTIVE=0
DOCTEST_IDS=()
DOCTEST_PIDS=()
DOCTEST_ACTIVE=0
NATIVE_IDS=()
NATIVE_PIDS=()
NATIVE_ACTIVE=0

stop_bg_pids() {
    local pid attempt alive

    # `start_bg` gives every wrapper and all of its descendants a
    # dedicated process group. Signal the whole group so interrupting
    # the gate cannot orphan Cargo, rustc, rustdoc, or test processes.
    for pid in "$@"; do
        kill -TERM -- "-$pid" 2>/dev/null || true
    done

    # Give well-behaved children two seconds to clean up, then bound
    # shutdown time by force-stopping any group that ignored TERM.
    attempt=0
    while [ "$attempt" -lt 20 ]; do
        alive=0
        for pid in "$@"; do
            if kill -0 -- "-$pid" 2>/dev/null; then
                alive=1
                break
            fi
        done
        if [ "$alive" -eq 0 ]; then
            break
        fi
        sleep 0.1
        attempt=$((attempt + 1))
    done

    for pid in "$@"; do
        if kill -0 -- "-$pid" 2>/dev/null; then
            kill -KILL -- "-$pid" 2>/dev/null || true
        fi
        wait "$pid" 2>/dev/null || true
    done
}

cleanup_bg() {
    if [ "$STATIC_ACTIVE" -eq 1 ]; then
        stop_bg_pids "${STATIC_PIDS[@]}"
    fi
    if [ "$DOC_ACTIVE" -eq 1 ]; then
        stop_bg_pids "${DOC_PIDS[@]}"
    fi
    if [ "$TEST_ACTIVE" -eq 1 ]; then
        stop_bg_pids "${TEST_PIDS[@]}"
    fi
    if [ "$DOCTEST_ACTIVE" -eq 1 ]; then
        stop_bg_pids "${DOCTEST_PIDS[@]}"
    fi
    if [ "$NATIVE_ACTIVE" -eq 1 ]; then
        stop_bg_pids "${NATIVE_PIDS[@]}"
    fi
    rm -rf "$BG_DIR"
}
trap cleanup_bg EXIT
trap 'exit 130' INT TERM

bg_step() {
    local idx="$1"
    shift 2
    local start=$SECONDS

    # Run the command in the wrapper's foreground so it inherits the
    # dedicated process group created by `start_bg`.
    if "$@" >"$BG_DIR/$idx.log" 2>&1; then
        printf 'ok' >"$BG_DIR/$idx.status"
    else
        printf 'fail' >"$BG_DIR/$idx.status"
    fi
    printf '%s' "$((SECONDS - start))" >"$BG_DIR/$idx.time"
}

start_bg() {
    local group="$1"
    local desc="$2"
    shift 2
    local idx pid
    BG_NEXT_ID=$((BG_NEXT_ID + 1))
    idx=$BG_NEXT_ID
    printf '%s' "$desc" >"$BG_DIR/$idx.desc"

    # Bash monitor mode assigns an asynchronous job its own process
    # group even in this non-interactive script. Disable it immediately
    # after launch so foreground gate commands retain normal semantics.
    set -m
    bg_step "$idx" "$desc" "$@" &
    pid=$!
    set +m
    case "$group" in
        static)
            STATIC_IDS+=("$idx")
            STATIC_PIDS+=("$pid")
            STATIC_ACTIVE=1
            ;;
        docs)
            DOC_IDS+=("$idx")
            DOC_PIDS+=("$pid")
            DOC_ACTIVE=1
            ;;
        test)
            TEST_IDS+=("$idx")
            TEST_PIDS+=("$pid")
            TEST_ACTIVE=1
            ;;
        doctest)
            DOCTEST_IDS+=("$idx")
            DOCTEST_PIDS+=("$pid")
            DOCTEST_ACTIVE=1
            ;;
        native)
            NATIVE_IDS+=("$idx")
            NATIVE_PIDS+=("$pid")
            NATIVE_ACTIVE=1
            ;;
        *)
            echo "internal error: unknown background group '$group'" >&2
            return 2
            ;;
    esac
}

report_bg_ids() {
    local idx desc status elapsed
    for idx in "$@"; do
        desc=$(cat "$BG_DIR/$idx.desc")
        status=$(cat "$BG_DIR/$idx.status" 2>/dev/null || echo fail)
        elapsed=$(cat "$BG_DIR/$idx.time" 2>/dev/null || echo '?')
        if [ "$status" = ok ]; then
            printf '✓ %s  (%ss)\n' "$desc" "$elapsed"
        else
            printf '✗ %s  (%ss)\n' "$desc" "$elapsed"
            cat "$BG_DIR/$idx.log"
            save_failure_log "$desc" "$BG_DIR/$idx.log"
            failed=1
        fi
    done
}

join_static_group() {
    local pid
    for pid in "${STATIC_PIDS[@]}"; do
        wait "$pid" || true
    done
    STATIC_ACTIVE=0
    report_bg_ids "${STATIC_IDS[@]}"
}

join_doc_group() {
    local pid
    for pid in "${DOC_PIDS[@]}"; do
        wait "$pid" || true
    done
    DOC_ACTIVE=0
    report_bg_ids "${DOC_IDS[@]}"
}

join_test_group() {
    local pid
    for pid in "${TEST_PIDS[@]}"; do
        wait "$pid" || true
    done
    TEST_ACTIVE=0
    report_bg_ids "${TEST_IDS[@]}"
}

join_doctest_group() {
    local pid
    for pid in "${DOCTEST_PIDS[@]}"; do
        wait "$pid" || true
    done
    DOCTEST_ACTIVE=0
    report_bg_ids "${DOCTEST_IDS[@]}"
}

join_native_group() {
    local pid
    for pid in "${NATIVE_PIDS[@]}"; do
        wait "$pid" || true
    done
    NATIVE_ACTIVE=0
    report_bg_ids "${NATIVE_IDS[@]}"
}

# `cargo fmt --check` is the verification pass; when --fix was given
# the apply-mode `cargo fmt --all` already ran before these jobs.
start_bg static "cargo fmt --all -- --check" cargo fmt --all -- --check
if [ "$FUZZ_GATE" -eq 1 ]; then
    start_bg static \
        "cargo fmt standalone fuzz workspace" \
        cargo fmt --manifest-path dstar-gateway-core/fuzz/Cargo.toml \
        --all -- --check
    start_bg static \
        "cargo clippy standalone fuzz workspace" \
        env CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=2 \
        cargo clippy --manifest-path dstar-gateway-core/fuzz/Cargo.toml \
        --all-targets -- -D warnings -D unsafe-code
fi

# Refresh one persistent advisory database, then inspect both already-resolved
# lockfiles from the same immutable snapshot. Running two default cargo-audit
# invocations concurrently would make both mutate ~/.cargo/advisory-db.
check_cargo_audits() {
    local advisory_db="$PWD/target/lint-advisory-db"

    cargo audit --db "$advisory_db" --file Cargo.lock || return 1
    if [ "$FUZZ_GATE" -eq 1 ]; then
        cargo audit --db "$advisory_db" --no-fetch \
            --file dstar-gateway-core/fuzz/Cargo.lock
    fi
}

start_bg static "cargo audit resolved lockfiles" check_cargo_audits
start_bg static "cargo deny check" cargo deny check
if [ "$FUZZ_GATE" -eq 1 ]; then
    start_bg static \
        "cargo deny standalone fuzz workspace" \
        cargo deny --manifest-path dstar-gateway-core/fuzz/Cargo.toml check
fi

# `--skip-target-dir` avoids trybuild scratch manifests and concurrent
# walks through the Cargo/doc/doctest artifact trees.
start_bg static "cargo machete --skip-target-dir ." cargo machete --skip-target-dir .

check_shell_scripts() {
    git ls-files -z '*.sh' | xargs -0 shellcheck
}

check_toml_format() {
    git ls-files -z '*.toml' | xargs -0 taplo fmt --check
}

start_bg static "shellcheck all tracked shell scripts" check_shell_scripts
start_bg static "taplo fmt --check all tracked TOML" check_toml_format

if [ "$BOOK_BUILD" -eq 1 ]; then
    start_bg static "mdbook build dstar-gateway/book" mdbook build dstar-gateway/book
fi

# Cargo processes do not share a jobserver. Divide LINT_JOBS between Clippy
# and test compilation in phase one, then between both rustdoc lanes in phase
# two. The small fuzz workspace is capped at two jobs above; native cross-builds
# use the separate LINT_XCODE_JOBS cap. LINT_JOBS remains the escape hatch for
# constrained or larger builders.
detect_cpu_count() {
    local count=""
    if command -v getconf >/dev/null 2>&1; then
        count=$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)
    fi
    if ! printf '%s' "$count" | grep -Eq '^[1-9][0-9]*$' && \
        command -v nproc >/dev/null 2>&1; then
        count=$(nproc 2>/dev/null || true)
    fi
    if ! printf '%s' "$count" | grep -Eq '^[1-9][0-9]*$'; then
        count=4
    fi
    printf '%s\n' "$count"
}

TOTAL_JOBS=${LINT_JOBS:-$(detect_cpu_count)}
if ! printf '%s' "$TOTAL_JOBS" | grep -Eq '^[1-9][0-9]*$'; then
    echo "LINT_JOBS must be a positive integer (got '$TOTAL_JOBS')" >&2
    exit 2
fi

XCODE_JOBS=${LINT_XCODE_JOBS:-2}
if ! printf '%s' "$XCODE_JOBS" | grep -Eq '^[1-9][0-9]*$'; then
    echo "LINT_XCODE_JOBS must be a positive integer (got '$XCODE_JOBS')" >&2
    exit 2
fi

# Phase one overlaps the base test suite with Clippy, whose compiler jobs are
# capped at roughly two thirds of the host. Phase two divides the machine
# evenly between default and all-feature rustdoc lanes while the small feature
# test matrix runs. Smaller hosts use serial docs with the full budget.
PARALLEL_DOCS=0
if [ "$TOTAL_JOBS" -ge 4 ]; then
    PARALLEL_DOCS=1
    DOC_JOBS=$((TOTAL_JOBS / 2))
    if [ "$DOC_JOBS" -lt 1 ]; then DOC_JOBS=1; fi
    MAIN_JOBS=$(((TOTAL_JOBS * 2) / 3))
    if [ "$MAIN_JOBS" -lt 1 ]; then MAIN_JOBS=1; fi
else
    DOC_JOBS=$TOTAL_JOBS
    MAIN_JOBS=$TOTAL_JOBS
fi
TEST_BUILD_JOBS=$((TOTAL_JOBS - MAIN_JOBS))
if [ "$TEST_BUILD_JOBS" -lt 1 ]; then TEST_BUILD_JOBS=1; fi
TEST_ENV+=(CARGO_BUILD_JOBS="$TEST_BUILD_JOBS")

# The default and all-feature docs use distinct target directories because
# their feature variants can collide. In the full gate, explicitly selecting
# both libraries and binaries includes pulsar's and stargazer's same-name bins,
# which Cargo's implicit selection otherwise skips. Scoped bin-only packages
# retain Cargo's implicit selection because `--lib` would be an error there.
doc_default() {
    if [ -z "$PKG" ]; then
        env RUSTDOCFLAGS="-D warnings" CARGO_PROFILE_DEV_DEBUG=0 \
            cargo doc --workspace --lib --bins --no-deps \
            --document-private-items --jobs "$DOC_JOBS" \
            --target-dir target/doc-gate/default
    elif [ "$PKG" = "pulsar" ] || [ "$PKG" = "stargazer" ]; then
        env RUSTDOCFLAGS="-D warnings" CARGO_PROFILE_DEV_DEBUG=0 \
            cargo doc -p "$PKG" --lib --bins --no-deps \
            --document-private-items --jobs "$DOC_JOBS" \
            --target-dir target/doc-gate/default
    else
        env RUSTDOCFLAGS="-D warnings" CARGO_PROFILE_DEV_DEBUG=0 \
            cargo doc "${SCOPE[@]}" --no-deps --document-private-items \
            --jobs "$DOC_JOBS" --target-dir target/doc-gate/default
    fi
}

doc_all_features() {
    if [ -z "$PKG" ]; then
        env RUSTDOCFLAGS="-D warnings" CARGO_PROFILE_DEV_DEBUG=0 \
            cargo doc --workspace --lib --bins --no-deps \
            --document-private-items --all-features \
            --jobs "$DOC_JOBS" --target-dir target/doc-gate/all-features
    elif [ "$PKG" = "pulsar" ] || [ "$PKG" = "stargazer" ]; then
        env RUSTDOCFLAGS="-D warnings" CARGO_PROFILE_DEV_DEBUG=0 \
            cargo doc -p "$PKG" --lib --bins --no-deps \
            --document-private-items --all-features --jobs "$DOC_JOBS" \
            --target-dir target/doc-gate/all-features
    else
        env RUSTDOCFLAGS="-D warnings" CARGO_PROFILE_DEV_DEBUG=0 \
            cargo doc "${SCOPE[@]}" --no-deps --document-private-items \
            --all-features --jobs "$DOC_JOBS" \
            --target-dir target/doc-gate/all-features
    fi
}

# ---------- native Apple application gates ----------
# Xcode projects, UniFFI Swift bindings, and XCFrameworks are generated
# artifacts. Rebuild the complete three-slice frameworks before testing so
# an old ignored artifact can never make current Rust/Swift integration pass.
# Two project lanes overlap the Rust gate, while commands inside a project
# stay serial to share one DerivedData cache without build.db contention.

check_azimuth_generated_plists() {
    local tmp mismatch=0
    tmp=$(mktemp -d)
    cp azimuth/project.yml "$tmp/project.yml"
    touch "$tmp/project-local.yml"
    cp -R azimuth/Shared azimuth/iPad azimuth/macOS azimuth/Tests \
        azimuth/Driver "$tmp/"
    if ! xcodegen generate --quiet --only-plists \
        --spec "$tmp/project.yml" --project-root "$tmp" --project "$tmp"; then
        rm -rf "$tmp"
        return 1
    fi
    for plist in iPad/Info.plist macOS/Info.plist; do
        if ! cmp -s "$tmp/$plist" "azimuth/$plist"; then
            echo "ERROR: azimuth/$plist is stale; regenerate it with xcodegen"
            mismatch=1
        fi
    done
    rm -rf "$tmp"
    return "$mismatch"
}

check_lodestar_generated_plists() {
    local tmp mismatch=0
    tmp=$(mktemp -d)
    cp lodestar/project.yml "$tmp/project.yml"
    touch "$tmp/project-local.yml"
    cp -R lodestar/Shared lodestar/iPad lodestar/macOS lodestar/Tests \
        lodestar/Driver lodestar/USBProbe "$tmp/"
    if ! xcodegen generate --quiet --only-plists \
        --spec "$tmp/project.yml" --project-root "$tmp" --project "$tmp"; then
        rm -rf "$tmp"
        return 1
    fi
    for plist in iPad/Info.plist macOS/Info.plist USBProbe/Info.plist; do
        if ! cmp -s "$tmp/$plist" "lodestar/$plist"; then
            echo "ERROR: lodestar/$plist is stale; regenerate it with xcodegen"
            mismatch=1
        fi
    done
    rm -rf "$tmp"
    return "$mismatch"
}

build_and_test_azimuth() {
    check_azimuth_generated_plists || return 1
    env CARGO_BUILD_JOBS="$XCODE_JOBS" \
        ./azimuth-core/scripts/build-xcframework.sh || return 1
    (cd azimuth && xcodegen generate --quiet --use-cache \
        --cache-path ../target/xcodegen-cache/azimuth) || return 1
    xcodebuild test \
        -project azimuth/Azimuth.xcodeproj \
        -scheme AzimuthMac \
        -destination 'platform=macOS' \
        -derivedDataPath target/xcode-derived/azimuth \
        -jobs "$XCODE_JOBS" \
        COMPILER_INDEX_STORE_ENABLE=NO || return 1
    xcodebuild build \
        -project azimuth/Azimuth.xcodeproj \
        -scheme AzimuthIPad \
        -destination 'generic/platform=iOS' \
        -derivedDataPath target/xcode-derived/azimuth \
        -jobs "$XCODE_JOBS" \
        COMPILER_INDEX_STORE_ENABLE=NO \
        CODE_SIGNING_ALLOWED=NO \
        CODE_SIGNING_REQUIRED=NO \
        CODE_SIGN_IDENTITY= \
        DEVELOPMENT_TEAM=
}

build_and_test_lodestar() {
    check_lodestar_generated_plists || return 1
    env CARGO_BUILD_JOBS="$XCODE_JOBS" \
        ./lodestar-core/scripts/build-xcframework.sh || return 1
    (cd lodestar && xcodegen generate --quiet --use-cache \
        --cache-path ../target/xcodegen-cache/lodestar) || return 1
    xcodebuild test \
        -project lodestar/Lodestar.xcodeproj \
        -scheme LodestarMac \
        -destination 'platform=macOS' \
        -derivedDataPath target/xcode-derived/lodestar \
        -jobs "$XCODE_JOBS" \
        COMPILER_INDEX_STORE_ENABLE=NO || return 1
    xcodebuild build \
        -project lodestar/Lodestar.xcodeproj \
        -scheme LodestarIPad \
        -destination 'generic/platform=iOS' \
        -derivedDataPath target/xcode-derived/lodestar \
        -jobs "$XCODE_JOBS" \
        COMPILER_INDEX_STORE_ENABLE=NO \
        CODE_SIGNING_ALLOWED=NO \
        CODE_SIGNING_REQUIRED=NO \
        CODE_SIGN_IDENTITY= \
        DEVELOPMENT_TEAM= || return 1
    xcodebuild build \
        -project lodestar/Lodestar.xcodeproj \
        -scheme USBProbe \
        -destination 'generic/platform=iOS' \
        -derivedDataPath target/xcode-derived/lodestar \
        -jobs "$XCODE_JOBS" \
        COMPILER_INDEX_STORE_ENABLE=NO \
        CODE_SIGNING_ALLOWED=NO \
        CODE_SIGNING_REQUIRED=NO \
        CODE_SIGN_IDENTITY= \
        DEVELOPMENT_TEAM=
}

if [ "$AZIMUTH_APP_GATE" -eq 1 ]; then
    start_bg native "Azimuth macOS tests + iPad/DriverKit build" \
        build_and_test_azimuth
fi
if [ "$LODESTAR_APP_GATE" -eq 1 ]; then
    start_bg native "Lodestar macOS tests + iPad/DriverKit/USBProbe builds" \
        build_and_test_lodestar
fi

# ---------- cargo gates ----------

# The executable suite has its own artifact cache. Its compile jobs are capped
# to leave Clippy headroom, while test execution overlaps Clippy. Join it before
# feature variants reuse the same target directory.
start_bg test \
    "cargo nextest run ${SCOPE[*]} --all-targets" \
    "${TEST_ENV[@]}" CARGO_TARGET_DIR="$TEST_TARGET_DIR" \
    cargo nextest run "${SCOPE[@]}" --all-targets

# Clippy: every target in scope. `--all-targets` covers tests,
# examples, and benches; the scope is the workspace by default or a
# single `-p` package for the edit loop.
run "${CLIPPY_ENV[@]}" cargo clippy "${SCOPE[@]}" --all-targets --jobs "$MAIN_JOBS" \
    --target-dir "$CLIPPY_TARGET_DIR" -- -D warnings

# Clippy again with every feature on. `--all-targets` does NOT reach a
# target whose `required-features` are disabled; Cargo skips those
# silently, with no warning. Without this pass the network-gated
# dstar-gateway examples, the hardware-tests integration test, and the
# thd75-repl `testing` module are never compiled by any gate, and rot
# undetected.
run "${CLIPPY_ENV[@]}" cargo clippy "${SCOPE[@]}" --all-targets --all-features --jobs "$MAIN_JOBS" \
    --target-dir "$CLIPPY_TARGET_DIR" -- -D warnings

# Feature-specific clippy: `--all-features` turns on `kenwood-tables`,
# which implies `encoder`, so the encoder-only combination is still
# unexercised above. Run each advertised feature set on its own.
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run "${CLIPPY_ENV[@]}" cargo clippy -p mbelib-rs --all-targets --features encoder \
        --jobs "$MAIN_JOBS" --target-dir "$CLIPPY_TARGET_DIR" -- -D warnings
    run "${CLIPPY_ENV[@]}" cargo clippy -p mbelib-rs --all-targets --features kenwood-tables \
        --jobs "$MAIN_JOBS" --target-dir "$CLIPPY_TARGET_DIR" -- -D warnings
fi

# Clippy is complete. On normal hosts, start both isolated documentation
# variants immediately so they can overlap the tail of the base test suite and
# the small feature-test matrix. Constrained hosts run docs serially later.
if [ "$PARALLEL_DOCS" -eq 1 ]; then
    start_bg docs \
        "cargo doc ${SCOPE[*]} (default, all libraries and binaries)" \
        doc_default
    start_bg docs \
        "cargo doc ${SCOPE[*]} --all-features (all libraries and binaries)" \
        doc_all_features
fi

# Supplemental feature tests reuse the base test target, so join that lane
# before starting them.
join_test_group

# Executable tests via cargo-nextest: the workspace has ~100 separate
# test binaries and `cargo test` executes them strictly one at a time;
# nextest schedules every test across all cores. `--all-targets` is important:
# six TH-D75 examples carry synthetic test harnesses whose `cfg(test)` code is
# otherwise missed by Clippy's normal example compilation. The manual scripts
# under `thd75/probes/` are deliberately not Cargo targets: many of their test
# functions write real radios, so the automatic gate only source-audits them.
# Doctests are not run by nextest and get their own pass below.
#
# The default pass runs no features, so any test target behind
# `required-features` is skipped and must be run explicitly here. This
# is NOT `--all-features` (as the clippy pass is): enabling every
# feature would turn on `dstar-gateway/hardware-tests`, whose test
# opens live reflectors, and `examples-network`. Each test-gating
# feature is run on its own crate instead. thd75-repl's `testing`
# feature gates the `script_integration` suite. mbelib-rs's
# kenwood-tables feature implies encoder, so encoder-only still needs
# its own run. Its wave-enhance tests are already selected by the
# workspace pass because sextant enables that feature.
if [ -z "$PKG" ] || [ "$PKG" = "thd75-repl" ]; then
    run "${TEST_ENV[@]}" CARGO_TARGET_DIR="$TEST_TARGET_DIR" \
        cargo nextest run -p thd75-repl --features testing --all-targets
fi
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run "${TEST_ENV[@]}" CARGO_TARGET_DIR="$TEST_TARGET_DIR" \
        cargo nextest run -p mbelib-rs --features encoder --all-targets
    run "${TEST_ENV[@]}" CARGO_TARGET_DIR="$TEST_TARGET_DIR" \
        cargo nextest run -p mbelib-rs --features kenwood-tables --all-targets
fi
if [ "$DSTAR_BLOCKING_MATRIX" -eq 1 ]; then
    run "${TEST_ENV[@]}" CARGO_TARGET_DIR="$TEST_TARGET_DIR" \
        cargo nextest run -p dstar-gateway --features blocking --all-targets
fi

if [ "$PARALLEL_DOCS" -eq 1 ]; then
    join_doc_group
else
    run_inline "cargo doc ${SCOPE[*]} (default, all libraries and binaries)" \
        doc_default
    run_inline "cargo doc ${SCOPE[*]} --all-features (all libraries and binaries)" \
        doc_all_features
fi

# Doctests. Cargo/rustdoc cannot cache their merged test executables,
# and one workspace command runs packages serially. Cargo also has no
# doctest partition flag.
#
# On POSIX-native hosts, every shard still selects `--workspace`,
# preserving the original command's exact cross-package feature
# unification and automatic new-member coverage. A transparent RUSTDOC
# wrapper hashes `--crate-name` and executes each `--test` unit in
# exactly one shard. Distinct persistent target directories remove
# Cargo's build lock and retain each shard's dependency cache between
# lint runs.
doctest_shard() {
    local shard_idx="$1"
    local shard_count="$2"
    local shard_jobs="$3"
    local target_dir="target/doctest-gate/shard-$shard_idx"
    local shard_failed=0

    env \
        CARGO_PROFILE_TEST_DEBUG=0 \
        CARGO_PROFILE_DEV_DEBUG=0 \
        RUSTDOC="$PWD/.config/doctest-shard-rustdoc.sh" \
        KENWOOD_REAL_RUSTDOC="$DOCTEST_REAL_RUSTDOC" \
        KENWOOD_DOCTEST_SHARD_INDEX="$shard_idx" \
        KENWOOD_DOCTEST_SHARD_COUNT="$shard_count" \
        cargo test --doc --workspace --jobs "$shard_jobs" \
        --target-dir "$target_dir" || shard_failed=1

    # Keep the two mbelib feature combinations from the original gate.
    # They follow shards 0 and 1, so they add no extra concurrent lane.
    if [ "$MBELIB_MATRIX" -eq 1 ] && [ "$shard_idx" -eq 0 ]; then
        env CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 \
        cargo test --doc -p mbelib-rs --features encoder \
            --jobs "$shard_jobs" --target-dir "$target_dir" || shard_failed=1
    fi
    if [ "$MBELIB_MATRIX" -eq 1 ] && \
        { [ "$shard_idx" -eq 1 ] || \
          { [ "$shard_count" -eq 1 ] && [ "$shard_idx" -eq 0 ]; }; }; then
        env CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 \
        cargo test --doc -p mbelib-rs --features kenwood-tables \
            --jobs "$shard_jobs" --target-dir "$target_dir" || shard_failed=1
    fi

    return "$shard_failed"
}

# An unsharded run uses a persistent scoped doctest directory. A scoped
# package has nothing to partition; this is also the compatibility path
# for native Windows Cargo.
#
# `cargo test --doc -p <bin-only-pkg>` hard-errors ("no library
# targets found") instead of no-opping, which would break the -p edit
# loop on bin-only crates (sextant). Tolerate exactly that error and
# report the skip; anything else is a real failure.
doc_tests_unsharded() {
    local out
    if out=$(env CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 \
        cargo test --doc "${SCOPE[@]}" \
        --target-dir target/doctest-gate/scoped 2>&1); then
        printf '%s\n' "$out"
        return 0
    fi
    if printf '%s' "$out" | grep -q 'no library targets found'; then
        printf '%s\n' "$out"
        echo "(bin-only package: no lib target, so doctests are skipped)"
        return 0
    fi
    printf '%s\n' "$out"
    return 1
}

run_unsharded_doctests() {
    run_inline "cargo test --doc ${SCOPE[*]}" doc_tests_unsharded
    if [ "$MBELIB_MATRIX" -eq 1 ]; then
        run "${TEST_ENV[@]}" cargo test --doc -p mbelib-rs \
            --features encoder --target-dir target/doctest-gate/scoped
        run "${TEST_ENV[@]}" cargo test --doc -p mbelib-rs \
            --features kenwood-tables --target-dir target/doctest-gate/scoped
    fi
    if [ "$DSTAR_BLOCKING_MATRIX" -eq 1 ]; then
        run "${TEST_ENV[@]}" cargo test --doc -p dstar-gateway \
            --features blocking --target-dir target/doctest-gate/scoped
    fi
}

# Native Windows Cargo cannot execute a shebang-only RUSTDOC wrapper,
# even when lint.sh itself is running under Git Bash. Keep the exact
# original serial commands there. A single requested/available shard
# also uses this path because partitioning would add no concurrency.
DOCTEST_SHARDING_SUPPORTED=1
case "$HOST_KERNEL" in
    CYGWIN*|MINGW*|MSYS*) DOCTEST_SHARDING_SUPPORTED=0 ;;
esac

if [ -n "$PKG" ] || [ "$DOCTEST_SHARDING_SUPPORTED" -eq 0 ]; then
    run_unsharded_doctests
else
    REQUESTED_DOCTEST_SHARDS=${LINT_DOCTEST_JOBS:-4}
    if ! printf '%s' "$REQUESTED_DOCTEST_SHARDS" | grep -Eq '^[1-8]$'; then
        echo "LINT_DOCTEST_JOBS must be an integer from 1 to 8" >&2
        failed=1
    else
        DOCTEST_SHARDS=$REQUESTED_DOCTEST_SHARDS
        if [ "$DOCTEST_SHARDS" -gt "$TOTAL_JOBS" ]; then
            DOCTEST_SHARDS=$TOTAL_JOBS
        fi

        if [ "$DOCTEST_SHARDS" -eq 1 ]; then
            run_unsharded_doctests
        else
            if [ -n "${RUSTDOC:-}" ]; then
                DOCTEST_REAL_RUSTDOC=$RUSTDOC
            else
                DOCTEST_REAL_RUSTDOC=$(command -v rustdoc)
            fi
            DOCTEST_CARGO_JOBS=$((TOTAL_JOBS / DOCTEST_SHARDS))
            shard=0
            while [ "$shard" -lt "$DOCTEST_SHARDS" ]; do
                start_bg doctest \
                    "cargo test --doc workspace shard $((shard + 1))/$DOCTEST_SHARDS" \
                    doctest_shard "$shard" "$DOCTEST_SHARDS" "$DOCTEST_CARGO_JOBS"
                shard=$((shard + 1))
            done
            join_doctest_group
            if [ "$DSTAR_BLOCKING_MATRIX" -eq 1 ]; then
                run "${TEST_ENV[@]}" cargo test --doc -p dstar-gateway \
                    --features blocking \
                    --target-dir target/doctest-gate/dstar-blocking
            fi
        fi
    fi
fi

# The native lanes started before Clippy and have overlapped the entire Rust
# gate. Collect them only after doctests so first-time Apple cross-compiles do
# not delay unrelated Rust diagnostics.
if [ "$NATIVE_ACTIVE" -eq 1 ]; then
    join_native_group
fi

# ---------- join background statics ----------
# mdbook / fmt-check / audit / deny / machete / shellcheck / taplo ran
# concurrently with the cargo chain; collect their verdicts.
join_static_group

# ---------- summary ----------
echo
if [ "$failed" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "OK"
