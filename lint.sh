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
mbelib-rs feature matrix), tests (cargo-nextest for executable
tests, cargo test --doc for doctests), docs, fmt, cargo-audit,
cargo-deny, cargo-machete. Steps that never touch the cargo
build lock (fmt check, audit, deny, machete, shellcheck, taplo,
mdbook) run in the background alongside the cargo chain and
report after it; their output is buffered, not streamed.

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
if [ "$FIX" -eq 1 ]; then
    run cargo fmt --all
    run cargo clippy --fix --allow-dirty "${SCOPE[@]}" --all-targets
    run cargo clippy --fix --allow-dirty "${SCOPE[@]}" --all-targets --all-features
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
# target; probes/examples/tests are separate compilation units. This
# audit closes those gaps by scanning the tree directly:
#
# 1. `unsafe_code` suppression markers: `allow(unsafe_code)` or
#    `expect(unsafe_code)` in any form, including the multi-line
#    attribute style where `unsafe_code,` sits on its own line.
# 2. The `unsafe` keyword itself (blocks, fns, extern, attributes).
#    Comment-only lines are excluded; a mention inside a string still
#    trips the audit; a loud false positive beats a silent miss.
#
# Allowlist (each entry is a deliberate design decision):
#   thd75/src/transport/bluetooth.rs: IOBluetooth RFCOMM FFI
#   thd75-tui/src/main.rs:           CFRunLoop pump for IOBluetooth
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
    if [ ${#missing[@]} -gt 0 ]; then
        echo "ERROR: required lint-gate tools are missing: ${missing[*]}"
        echo "Install with:"
        for tool in "${missing[@]}"; do
            case "$tool" in
                shellcheck)    echo "  brew install shellcheck   # or: apt install shellcheck" ;;
                taplo)         echo "  cargo install taplo-cli --locked" ;;
                mdbook)        echo "  cargo install mdbook --locked" ;;
                cargo-nextest) echo "  cargo install cargo-nextest --locked" ;;
                *)             echo "  cargo install $tool" ;;
            esac
        done
        return 1
    fi
    return 0
}

run_inline "required tools present" check_required_tools

# ---------- background static group ----------
# None of these steps compile anything or take cargo's build-directory
# lock (fmt/machete/audit/deny read sources or Cargo.lock; shellcheck,
# taplo and mdbook don't invoke cargo at all), so they run concurrently
# with the cargo chain below instead of serially after it. Their output
# is captured per step and reported by join_static_group after the
# cargo chain finishes: buffered, never interleaved with cargo output.
#
# Each step writes four files into $BG_DIR keyed by index: .desc, .log,
# .status, .time. The parent reads those at join time; passing state
# through files sidesteps the classic "subshell assigns failed=1 and
# the parent never sees it" trap (see the ci_pod comment in
# ci-local.sh for the war story).
BG_DIR=$(mktemp -d)
BG_PID=""

cleanup_bg() {
    if [ -n "$BG_PID" ]; then
        kill "$BG_PID" 2>/dev/null || true
        wait "$BG_PID" 2>/dev/null || true
    fi
    rm -rf "$BG_DIR"
}
trap cleanup_bg EXIT
trap 'exit 130' INT TERM

bg_step() {
    local idx="$1"
    shift
    local start=$SECONDS
    printf '%s' "$*" >"$BG_DIR/$idx.desc"
    if "$@" >"$BG_DIR/$idx.log" 2>&1; then
        printf 'ok' >"$BG_DIR/$idx.status"
    else
        printf 'fail' >"$BG_DIR/$idx.status"
    fi
    printf '%s' "$((SECONDS - start))" >"$BG_DIR/$idx.time"
}

static_group() {
    # `cargo fmt --check` is the verification pass; when --fix was given
    # the apply-mode `cargo fmt --all` already ran in the foreground
    # before this group was spawned, so there is no mutation race.
    bg_step 1 cargo fmt --all -- --check

    # `cargo-audit` / `cargo-deny` both honour the workspace `Cargo.lock`
    # at the repo root (and fetch their advisory DBs into separate
    # directories under ~/.cargo, so running them back to back is safe).
    bg_step 2 cargo audit --file Cargo.lock
    bg_step 3 cargo deny check

    # `--skip-target-dir` is required, not cosmetic: the trybuild
    # compile-fail suites generate scratch crates under `target/` whose
    # manifests list every dev-dependency and use almost none of them.
    # Machete's directory walk only skips them by way of `.gitignore`,
    # which it honours ONLY inside a git checkout, so without this flag
    # the check passes here and fails anywhere the tree is built without
    # `.git` (the CI pods, which receive a tarball). It also keeps the
    # walk clear of the cargo chain writing target/ concurrently.
    bg_step 4 cargo machete --skip-target-dir .

    # Static-analyze the lint gate scripts themselves. Both files
    # contain trap handlers, kubectl heredocs, and pipefail-sensitive
    # pipelines; shellcheck catches the bash foot-guns those introduce.
    bg_step 5 shellcheck lint.sh ci-local.sh

    # Verify every workspace `Cargo.toml` is taplo-formatted. Catches
    # manifest drift (trailing whitespace, key reordering) before review.
    bg_step 6 taplo fmt --check Cargo.toml '*/Cargo.toml'

    # The mdBook is committed but nothing else compiles it. With
    # `create-missing = false` in book.toml, this fails when SUMMARY.md
    # names a chapter that was never written, instead of quietly
    # generating an empty placeholder for it.
    if [ "$BOOK_BUILD" -eq 1 ]; then
        bg_step 7 mdbook build dstar-gateway/book
    fi
}

static_group &
BG_PID=$!

join_static_group() {
    # Individual step statuses carry the outcome; the subshell itself
    # always exits 0.
    wait "$BG_PID" || true
    BG_PID=""
    local d idx desc status elapsed
    for d in "$BG_DIR"/*.desc; do
        [ -e "$d" ] || continue
        idx=$(basename "$d" .desc)
        desc=$(cat "$d")
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

# ---------- cargo gates ----------

# Clippy: every target in scope. `--all-targets` covers tests,
# examples, and benches; the scope is the workspace by default or a
# single `-p` package for the edit loop.
run cargo clippy "${SCOPE[@]}" --all-targets -- -D warnings

# Clippy again with every feature on. `--all-targets` does NOT reach a
# target whose `required-features` are disabled; Cargo skips those
# silently, with no warning. Without this pass the network-gated
# dstar-gateway examples, the hardware-tests integration test, and the
# thd75-repl `testing` module are never compiled by any gate, and rot
# undetected.
run cargo clippy "${SCOPE[@]}" --all-targets --all-features -- -D warnings

# Feature-specific clippy: `--all-features` turns on `kenwood-tables`,
# which implies `encoder`, so the encoder-only combination is still
# unexercised above. Run each advertised feature set on its own.
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run cargo clippy -p mbelib-rs --all-targets --features encoder -- -D warnings
    run cargo clippy -p mbelib-rs --all-targets --features kenwood-tables -- -D warnings
fi

# Executable tests via cargo-nextest: the workspace has ~100 separate
# test binaries and `cargo test` executes them strictly one at a time;
# nextest schedules every test across all cores. Coverage is identical
# for executable tests (lib/bin/integration, no custom harnesses in
# this workspace); doctests are NOT run by nextest and get their own
# pass below. Example and bench targets are compile-covered by the
# clippy `--all-targets` passes above.
#
# The default pass runs no features, so any test target behind
# `required-features` is skipped and must be run explicitly here. This
# is NOT `--all-features` (as the clippy pass is): enabling every
# feature would turn on `dstar-gateway/hardware-tests`, whose test
# opens live reflectors, and `examples-network`. Each test-gating
# feature is run on its own crate instead. thd75-repl's `testing`
# feature gates the `script_integration` suite; mbelib-rs's matrix follows
# the same per-crate rule as its clippy matrix (kenwood-tables implies encoder,
# so the encoder-only combination needs its own run).
run cargo nextest run "${SCOPE[@]}"
if [ -z "$PKG" ] || [ "$PKG" = "thd75-repl" ]; then
    run cargo nextest run -p thd75-repl --features testing
fi
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run cargo nextest run -p mbelib-rs --features encoder
    run cargo nextest run -p mbelib-rs --features kenwood-tables
fi

# Doctests. Cargo cannot cache these: every run recompiles each
# crate's merged doctest binary, serially, crate by crate; this is
# the warm gate's floor (~75s workspace-wide) and the reason the
# executable tests moved to nextest rather than everything.
#
# `cargo test --doc -p <bin-only-pkg>` hard-errors ("no library
# targets found") instead of no-opping, which would break the -p edit
# loop on bin-only crates (sextant). Tolerate exactly that error and
# report the skip; anything else is a real failure.
doc_tests_scoped() {
    local out
    if out=$(cargo test --doc "${SCOPE[@]}" 2>&1); then
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
run_inline "cargo test --doc ${SCOPE[*]}" doc_tests_scoped
if [ "$MBELIB_MATRIX" -eq 1 ]; then
    run cargo test --doc -p mbelib-rs --features encoder
    run cargo test --doc -p mbelib-rs --features kenwood-tables
fi

# Docs: scoped build with `-D warnings` so broken doc links hard-fail
# instead of printing a yellow warning. `--document-private-items` is
# load-bearing: rustdoc only resolves intra-doc links on items it
# actually renders, so a `[`RENAMED_CONST`]` link sitting on a private
# field is never checked without it.
#
# Each variant gets its OWN target dir. All three passes claim the
# same output paths under a shared target/doc, so in a shared dir each
# pass invalidates the previous variant's fingerprints and every
# lint.sh run re-renders all docs three times over, forever (~80s of
# pure thrash per run, measured on a fully warm cache). Separate dirs
# cost a one-time cold metadata build and a few GB of disk, then stay
# warm. Nested under target/ so gitignore and machete's
# --skip-target-dir cover them. These passes are lint-only; docs.yml
# still publishes from a plain `cargo doc` into target/doc.
RUSTDOCFLAGS="-D warnings" run cargo doc "${SCOPE[@]}" --no-deps --document-private-items --target-dir target/doc-gate/default

# Docs again with every feature on. Feature-gated modules carry doc
# comments too, and the default-feature pass above never renders them
# (mbelib-rs `encode::kenwood`, thd75-repl `mock_scenarios`), so their
# intra-doc links go unchecked.
RUSTDOCFLAGS="-D warnings" run cargo doc "${SCOPE[@]}" --no-deps --document-private-items --all-features --target-dir target/doc-gate/all-features

# Docs a third time over binary targets. When a package ships both a
# lib and a bin (thd75-repl), cargo's default target selection renders
# only the lib, so every doc comment in `main.rs` goes unchecked. Use
# `--bins` rather than `--lib --bins`: the latter hard-errors on
# bin-only packages (sextant). Lib-only packages no-op with a warning.
# Bin doc output collides with lib output filenames (thd75-repl), so
# this variant needs its own dir even relative to the all-features
# pass above.
RUSTDOCFLAGS="-D warnings" run cargo doc "${SCOPE[@]}" --no-deps --document-private-items --all-features --bins --target-dir target/doc-gate/bins

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
