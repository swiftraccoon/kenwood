# Kenwood radio workspace

Rust workspace for the Kenwood TH-D75 and TM-D750, a D-STAR reflector stack, packet-radio crates, and native iPadOS/macOS apps. Per-crate rules live in each crate's `AGENTS.md` and load only when files in that crate are touched; read a sibling crate's file explicitly when a change crosses crates.

## Standard

- These crates are the reference Rust implementation for the TH-D75 and TM-D750: usable without reading the source, safe by construction, and documented to the standard library's level. The bar is the highest level of professionalism found in any comparable project and nothing lower; difficulty, verbosity or slower delivery never justify relaxing a rule in this file.
- Linting and typing are as strict as the toolchain allows: every lint group in the Clippy policy stays denied, no suppression is added for convenience, and every boundary uses validated newtypes and enums that reject invalid values at construction, never raw integers or strings.
- `kenwood-thd75` and `kenwood-tmd750` carry the smallest dependency set that works, ideally none beyond the shared workspace crates. Every external crate must do something the standard library cannot and must meet the dependency rule under Workspace boundaries.
- Every public item has a rustdoc that states its contract; every README is the crate's published front page and is doc-tested wherever the toolchain allows.
- Every committed claim is checked before it lands: against the code for behavior, against the named firmware and interface for hardware facts, and against the cited upstream for protocol facts. A claim that cannot be checked is written as unverified, never asserted.
- Prose is plain and factual: no hype, no marketing, no superlatives, no filler.

## Process rules

- Experimental APIs prioritize correctness and clarity over compatibility: make breaking changes when they improve the design, migrate workspace consumers, and add no compatibility shims.
- No em dashes (U+2014) in committed prose: doc comments, code comments, markdown, shell/TOML/YAML comments, user-facing strings. Choose punctuation per sentence (semicolon, colon, comma pair, parentheses, or reword); never a mechanical hyphen swap. En dashes and box-drawing characters are fine. The generated manifests, generated registries and `mcp-d75-extract/tests/fixtures/` are exempt; `lint.sh` enforces the ban everywhere else.
- NEVER commit without being asked: stage, show the diff, wait for the word "commit". NEVER push before the user tests and confirms. NEVER `git add -A`; stage explicit paths. NEVER add Co-Authored-By lines.
- NEVER use bulk text replacement (sed, perl, awk, python one-liners, shell loops over files) on source or prose. Read each file and edit it individually; every bulk insertion in this codebase has broken a file.
- Library crates must be compiling, tested and lint-clean before their TUI or REPL consumers are touched.
- Run `./lint.sh` before every commit and `./ci-local.sh` before every push.
- Do not reflexively pipe output through `head` or `tail`; `grep` for what you need or redirect to a file.
- Commit messages describe behavior and policy, not the cleanup job: no line or test counts, no file paths, no "squash/consolidated" framing.
- Every commit subject starts with a scope prefix: the package name (`kenwood-tmd750:`, `tmd750-repl:`) or `docs:`, `ci:`, `build:`, `lint:`, `workspace:`, `README:` for workspace-level changes, followed by a lowercase description.
- Never reference gitignored or internal-only context from committed content (README, source comments, commit messages, PR descriptions): `CLAUDE.local.md`, `docs/`, `specs/`, `phases/`, `ref/`, internal ticket IDs, and local reverse-engineering tools or their endpoints. A gitignored directory that a shipped program creates as its default output (the REPL capture directory) may be documented as that program's output. Cite public upstreams de-pathed (see Reference hierarchy); the two carve-outs are listed there.
- A superseded finding must be corrected wherever it was written: the instruction file, the doc comment on the constant it produced, and anything that reasoned from it. Grep for the constant, not the prose.
- TM-D750 operator state: verify CAT state directly instead of asking for menu or screen confirmations. Require an independent control path (the control-panel USB endpoint) before any Terminal activation; never a same-interface escape or a power cycle. Never repeat an MCP session or bypass the uncertain-framing safeguards to work around a silent radio; a consumed one-shot approval does not cover another attempt.

## Documentation and comment register

Committed doc comments, READMEs and comments state the contract: inputs, units, deadlines, byte counts, frame sequences, error variants and their triggers, ownership and cancellation boundaries, and hardware facts with their qualifying firmware and interface. Say what the item is before any caveat.

- One scope statement per module or README section, never per item; delete every further "does not prove / does not establish" clause.
- Never document an item only by what it is not; a constant states what it bounds.
- Replace approval vocabulary ("separately approved", "operator approval", "grants retry authority") with the enforceable precondition: the flag, the state checked, or the error returned.
- Prefer the named mechanism over house vocabulary: "the open connection", "the fsynced journal record", "returns `Error::X`", not "owner", "durable intent", "evidence", "admission", "qualification".
- Comments state current behavior, not how it was reached: no first-person narrative, no superseded implementation, no fix rationale.
- Operator-facing strings carry state and next action only.
- Dated bench runs, approvals, capture paths, per-attempt timings and verification history belong in the gitignored `CLAUDE.local.md`; committed prose keeps only the tested configuration tuple and any timing or protocol fact a caller must handle.
- Instruction files (`AGENTS.md`, `CLAUDE.local.md`) hold rules, gotchas and non-generic commands only: no session narrative, no dated ledgers, no directory trees or module maps, and under 200 lines each.

## Commands

```bash
./lint.sh                   # full gate: unsafe audit, clippy --all-targets, nextest + doctests, docs, fmt, audit, deny, machete
./lint.sh -q                # quiet: only failures print
./lint.sh -q -p <package>   # edit-loop scope for one package; the full gate is still required before a commit
./lint.sh --fix             # cargo fmt --all and clippy --fix first
cargo nextest run -p <package> -E 'test(<substring>)'   # one test; -E 'binary(<target>)' for one test binary
cargo test -p <package> --doc                            # doctests; nextest never runs them
```

- Pin the toolchain for every cargo invocation, including `./lint.sh`: `PATH="$HOME/.rustup/toolchains/1.94.0-aarch64-apple-darwin/bin:$PATH"`. The Homebrew `cargo` first on PATH is newer and ignores `rust-toolchain.toml`; `rustup run 1.94.0` alone does not prevent a Homebrew clippy from being selected.
- Package names differ from directories for two crates: `-p kenwood-thd75` (not `thd75`) and `-p kenwood-tmd750` (not `tmd750`). `Cargo.lock` is gitignored; never stage it.
- Scoped clippy must match the gate: `cargo clippy -p <pkg> --all-targets -- -D warnings`. Without `-D warnings`, warn-level rustc lints pass locally and fail `lint.sh`.
- Never pipe `./lint.sh` before acting on its status: `lint.sh | tail -1 && git commit` commits on failure. Run it bare or with `set -o pipefail`.
- A background gate wrapper must `exit $rc` with the gate's status, and any commit chain must test that status, never merely print it.
- On failure `./lint.sh` writes `.lint-failures/<step>.log`; read that instead of rerunning. Old logs are never cleaned up, so only the steps marked ✗ in the current run are live.
- Two crates ship `examples/monitor.rs` (thd75, aprs-is) and collide in the shared target directory: always `cargo run -p <crate> --example monitor`, never `./target/debug/examples/monitor`.
- `-E 'test(<substring>)'` matches test names (module path included), never binary names: an integration binary whose tests lack the substring is skipped silently, so check the `N tests run` count or use `binary(<target>)`.
- Run `cargo fmt --all` once at the end of an edit batch: a mid-batch format rewrites anchors and forces every later edit to re-read its file.

## Workspace boundaries

- Shared crates (`kenwood-transport`, `kenwood-schema`, `mmdvm`) are imported canonically; model crates never re-export their types and there are no compatibility adapters.
- The two MCP live-session engines (TH-D75 and TM-D750) stay separate: framing, retry, entry/exit timing and recovery differ. Do not merge them.
- The two model libraries share many type names, but the types diverged by design (different `Frequency` domains, tone tables in f64 Hz vs u16 decihertz, field-parser semantics, per-crate `ProtocolError`); only the standard `DCS_CODES` array is byte-identical. Do not fold them into a shared CAT crate; the shared layer is `kenwood-schema`, `kenwood-transport` and `mmdvm`.
- `kenwood-tmd750` and `tmd750-repl` never depend on `kenwood-thd75`; seam tests in both crates enforce it.
- The packet-radio crates form a strict DAG: `aprs-is` depends on `aprs`; `aprs` depends on `ax25-codec` and `kiss-tnc`; those two are leaf crates with no workspace path dependencies. `kiss-tnc`, `ax25-codec` and `aprs` are sans-io and never read the clock; `aprs-is` is the tokio shell and the clock source.
- `unsafe_code = "forbid"` is workspace-wide. Hand-written unsafe is confined to `kenwood-transport/src/bluetooth.rs` and `thd75/src/screen/vision.rs`; `lodestar-core` and `azimuth-core` omit the Cargo lint only for generated UniFFI scaffolding. `lint.sh` audits this exact path list.
- New dependencies need large community support and many active maintainers.
- Tracked developer automation is Rust-only; `tools/` is untracked local scratch. Product sources also include the Swift/DriverKit Lodestar and Azimuth apps.

## Reference hierarchy

- Verified third-party sources (MMDVMHost, ircDDBGateway, dsdcc, mbelib, OP25, DStarRepeater, dsd-fme; public GitHub repos, cloned locally under gitignored `ref/`) are the reference for protocol behavior. In committed code cite them de-pathed: `MMDVMHost/Modem.cpp:42`, never `ref/...`.
- Non-public references (decompiled software, RE data, firmware images) are never cited in committed content, with two carve-outs: `mbelib-rs/src/encode/kenwood/` (firmware provenance for opaque tables, extracted under DMCA §1201(f)) and the MCP menu-schema pipeline (`mcp-d75-extract`, the generated manifests and registries, the "decompiled member name" phrase in `thd75/README.md`). The carve-outs cover provenance, not conclusions: a firmware-derived claim that proves wrong is corrected in the committed doc.
- Our own code is not a reference for firmware behavior; never justify a hypothesis with "Lodestar does X".
- Never reference D74 documentation; D74 assumptions have been wrong every time.
- The TM-D750 MCP text writer uses NUL padding; the TH-D75 space-padding override for `MyCallsignDvGateway` is D75-only.

## CI

- All platforms pin Rust 1.94.0; `rust-toolchain.toml` matches. Symptom of drift: dozens of new clippy lints and trybuild stderr mismatches that CI never sees.
- A new RUSTSEC advisory fails the gate: try `cargo update <crate>` first. With no semver-compatible fix, add paired documented ignores in both `.cargo/audit.toml` and `deny.toml`; duplicate-version `skip` entries pin exact versions and go stale on bumps.
- CI resolves a fresh lockfile, so a green local gate proves nothing about it. Reproduce cargo-deny duplicates with `cargo generate-lockfile` (back up `Cargo.lock` first) and `cargo deny check bans`.
- The persistent `target/clippy-gate` dir and CI's rust-cache reuse clippy fingerprints, so a pre-existing `-D warnings` violation (e.g. `missing_assert_message`) stays hidden until an edit to that module or its dependencies invalidates the fingerprint; a green gate is not proof the whole tree is clippy-clean.
- `gh run list --commit <sha>` returns nothing here; list unfiltered with `--json headSha` and filter. A scratch `git worktree` with CRLF-converted files stands in for the Windows runner.
- nextest flags tests `slow` when the first run after a source change includes the rebuild; rerun before diagnosing.
- Platform CI runs per-crate `cargo test` with a `working-directory` per crate so each crate compiles with only its own features; there are no separate `cargo check` steps. Docs publishing runs on macOS so native transport API pages exist; the publisher keeps implicit library-over-binary target selection. A successful local render is not a Pages deployment.
- KI4LAX spec comparisons in `thd75/tests/spec_audit.rs` are `#[ignore]`; run them with `THD75_KI4LAX_SPEC=/absolute/path.json cargo test -p kenwood-thd75 --test spec_audit -- --ignored`.
- `lint.sh` requires `cargo-audit`, `cargo-deny`, `cargo-machete`, `shellcheck`, `taplo` and `mdbook`; a missing tool fails the gate.
- Windows: D-STAR fixtures that touch directory durability go through the explicit durability seam, never a Unix directory open plus sync.
- `ci-local.sh` runs macOS locally plus Ubuntu and Fedora on k8s pods. Stuck `ci-ubuntu`/`ci-fedora` pods from a killed run must be force-deleted first; the script silences kubectl errors, so "no pod output" is the symptom. macOS `ar` warnings starting `warning: kenwood-thd75@` are harmless. zsh: `$pipestatus` is lowercase and unquoted variables do not word-split (use `xargs` or `${=var}`).
- `ci-local.sh` catches Linux-only `dead_code` the macOS `lint.sh` cannot: an item used only under `#[cfg(any(target_os = "macos", test))]` is dead in a Linux non-test build and fails `-D warnings`. Gate such helpers with the same `cfg`.
- `ci-local.sh` snapshots the tree with `git ls-files --cached`; stage tracked-file deletions (`git rm`) before running it, or its tar fails with `Cannot stat` on the removed paths.

## File naming

- `AGENTS.md` is the tracked, tool-neutral instruction file at the root and in each crate.
- The `CLAUDE.md` beside it holds only `@AGENTS.md`, the import Claude Code reads.
- `CLAUDE.local.md` is the gitignored operator file: pinned protocol facts, local tool setup, hardware checklist. Never reference it from committed content.

## Agents

- Subagent model is `opus` (Opus 5), never Fable. At most about ten agents per task: one editor per disjoint file group plus one synthesizer or verifier, never one agent per finding.
- Never unilaterally subset a plan; execute what was written or ask.
- After subagent work run `cargo fmt --all`, then verify the claimed scope on disk with `git diff --stat <path>`; agent reports describe intent, not outcome.
- Parallel editors own disjoint file sets and share the cargo target directory: run the scoped gate once at the end, and wait and retry (never edit) when a failure lands in another editor's file.
- A workflow interrupted by a usage-limit pause keeps running; before relaunching, check its `journal.jsonl` for missing `result` lines and `find . -mmin -15` for live writers.

## Documentation gates

- `cargo doc` resolves intra-doc links only on items it renders, so `lint.sh` runs `--document-private-items`.
- Implicit `cargo doc` selection prefers a library over its same-name binary (`thd75-repl` is the trap); the full gate selects `--lib --bins`. Do not add `--lib` to a bin-only package such as `sextant`.
- `cargo test --doc` never sees ```` ```text ```` or ```` ```ignore ```` fences, binary crates, or README prose. `thd75`, `tmd750`, `kenwood-transport`, `kenwood-schema`, `mmdvm`, `mbelib-rs`, `dstar-gateway` and `azimuth-core` include their README through `#![doc = include_str!]`, so README identifiers need backticks (`clippy::doc_markdown`), a fence that opens a port is ```` ```rust,no_run ````, and relative links must be absolute GitHub URLs. Those READMEs are the published API front pages.
- The only mdBook is `dstar-gateway/book`; its `book.toml` sets `create-missing = false`. mdbook does not validate inline `[text](page.md)` links; check them by hand when deleting a page.
- Generated registries (`thd75/src/memory/menu_fields.rs`, `tmd750/src/memory/menu_fields.rs`) and manifests are never hand-edited; change the generator templates in `mcp-d75-extract/src/rustgen.rs`, regenerate, and keep the untouched artifacts SHA-identical.

## Rust knowledge hygiene

- Never claim a feature is nightly-only or unstable without verifying; when unsure what the stdlib can do, write a ten-line test and run it.
- Local docs: the pinned 1.94.0 toolchain has no `rust-docs` component, so a bare `rustup doc` fails from the repo root. Use `rustup component add rust-docs --toolchain 1.94.0`, or `rustup doc --toolchain stable --std`, remembering that stable is ahead of the gate.
- Same discipline for platform APIs: read the primary Apple doc page before asserting that something is impossible on iOS or macOS.
- `#![expect(lint)]` is stable since 1.81; use it instead of `#![allow(lint)]` so a stale suppression fails.
- `fancy_regex` patterns must not open with `(?m)^\s*`; over long whitespace runs `\s*` backtracks until the limit trips. Anchor with `^[ \t]*`; the `regex` crate is unaffected.
- clap `ValueEnum` renders `PmName1` as `pm-name1` (no hyphen before a trailing digit); a variant that must match a library key such as `pm-name-1` needs `#[value(name = "...")]`.

## Clippy policy

- `clippy::all`, `pedantic`, `nursery` and `cargo` are denied in every crate, plus `missing_docs`, `elided_lifetimes_in_paths`, `trivial_casts` and `unused_qualifications`; clippy runs with `--all-targets`.
- Never add `#![allow(...)]` without explicit approval; `allow_attributes` is denied, so use `#[expect(<lint>, reason = "...")]`. If clippy reports the expectation unfulfilled, the suppression was unnecessary.
- Fix the code instead of suppressing: `buf.get(..n).expect("n within buf")` not `&buf[..n]`; `unreachable!` not `panic!`; `assert!(expr.is_err())` not `expect_err`; `HashSet<Enum>` instead of `struct_excessive_bools`.
- `#[cfg_attr(test, expect(...))]` is banned: it checks the build predicate, not that the item is a test.
- `dead_code` expectations cannot stage a new module: a module-level expect makes everything live, and an item-level expect on a test-only caller is unfulfilled in the test build. Wire a real consumer instead.
- `#[expect]` is evaluated per target, not per cfg block; split `#[cfg(target_os)]` branches into separate functions. `./lint.sh` runs on macOS only; `./ci-local.sh` catches Linux.
- `drop(map.insert(..))` on a `Copy` value is `dropping_copy_types`; bind it as `let _previous = ...`.

## Cargo `[lints]` inheritance

- A crate-level `[lints.<group>]` block replaces the workspace block entirely; `workspace = true` cannot be mixed with per-lint overrides. Override crates (`thd75`, `thd75-tui`, `thd75-repl`, `lodestar-core`, `azimuth-core`) restate every workspace lint and carry a "KNOWN DIVERGENCE FROM WORKSPACE" comment; add no new override without one.
- `unsafe_code = "forbid"` cannot be weakened at crate level from source; crates that need unsafe or generated FFI override `[lints.rust]` in Cargo.toml and still deny unsafe outside the audited files.
- `unused_crate_dependencies` fires per compilation unit: fix with `use <dep> as _;` in `src/main.rs`, every `tests/*.rs`, and `#[cfg(test)]` in `lib.rs` for dev-dependencies; never with a blanket allow.

## Test code

- Tests return `Result<(), Box<dyn std::error::Error>>` and use `?`; `.unwrap()`, `.expect()` and `panic!` are not used. `Option::expect` becomes `.ok_or("...")?`; `expect_err` becomes `assert!(matches!(result, Err(Variant)), "{result:?}")`.
- Fixtures use infallible constructors: `Callsign::from_wire_bytes(*b"W1AW    ")`, `Module::C`, `SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001)`, `StreamId::new(0x1234).unwrap()` in a `const fn` (const `Option::unwrap` panics at compile time).
- `type TestResult = std::result::Result<...>` only in crates whose `use super::*` imports a one-parameter `Result` alias; elsewhere the qualification trips `unused_qualifications`.
- Proptest: `fn to_test_err<E: Debug>(e: E) -> TestCaseError` with `.map_err(to_test_err)?`, and `.prop_filter_map("reason", |x| T::new(x).ok())` for validated types.
- When tests and implementation land together, prove the critical tests can fail: mutate the load-bearing behavior, watch the exact tests go red, restore byte-identically, rerun green.
- Const: `Option::unwrap` is const since 1.83; `Result::unwrap` is not (match with `unreachable!`). `const fn` cannot deref a `Vec` or call `try_from`; make such functions non-const rather than `as`-narrowing.
- A test whose only `?` is removed trips `unnecessary_wraps`; keep one real fallible step and bind its value (`let _baseline = parse(&arguments())?;`) so `unused_results` stays satisfied.
