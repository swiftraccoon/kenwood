# Contributing

The workspace follows [RUST_STANDARDS.md](RUST_STANDARDS.md). Read the root and
affected crates' `AGENTS.md` files before editing.

## Toolchain and checks

Use Rust 1.94.0 and its matching Clippy and rustfmt executables. On POSIX hosts,
put the selected toolchain's binaries first so another installed Clippy cannot
override it:

```sh
export PATH="$(rustup run 1.94.0 rustc --print sysroot)/bin:$PATH"
```

Install the gate's required tools before running it: `cargo-nextest`,
`cargo-audit`, `cargo-deny`, `cargo-machete`, `shellcheck`, `taplo` and `mdbook`.
On macOS, the full gate also needs Xcode and XcodeGen for native application
builds and tests. Run commands from the workspace root unless a crate requires
its own working directory.

```sh
./lint.sh -q -p ax25-codec
./lint.sh
```

The scoped command is an edit-loop check. The complete `./lint.sh` must pass
on the completed working tree before committing. It checks formatting, unsafe
boundaries, Clippy, executable tests, doctests, documentation, dependencies and
the additional project gates.
Inspect the failing step's saved log before rerunning it. Keep the tree unchanged
while the gate runs.

Reuse a passing result when splitting the same checked tree into focused
commits or rewriting its history. Check an intermediate snapshot when changed
commit boundaries introduce a new combination of APIs and consumers. Changed
checked files require validation again; commit metadata alone does not.

Run `./ci-local.sh` before every push, in addition to the macOS local gate.
Push only when instructed. Platform workflows also check isolated crates and
feature combinations; workspace dependency compilation does not execute a
dependency's own tests.

Publishing these workflow changes requires a separate merge-enforcement step:
require the portable gate and all three isolated platform jobs in the rules for
`main`, retaining any other required checks. Use the exact check names reported
by the first GitHub Actions run and verify that failures block merging.

The reviewable ruleset payload is
[`.github/rulesets/rust-standards.json`](.github/rulesets/rust-standards.json).
It requires the current branch to include the latest base and binds the four
checks to GitHub Actions. Add it as a separate ruleset so existing rules remain
effective. After publishing and observing the named checks, an administrator can
apply it with `gh api --method POST repos/{owner}/{repo}/rulesets --input
.github/rulesets/rust-standards.json`. Inspect the active rules with `gh api
repos/{owner}/{repo}/rules/branches/main` and verify a failing check leaves a pull
request blocked. A tracked payload alone does not activate repository rules.

## Change workflow

1. Identify affected public APIs, invariants, consumers, features and platforms.
   Use the adopted rule identifiers in design and review notes.
2. Make focused changes. Keep mechanical formatting separate from behavior and
   API changes. Change generated artifacts through their generator.
3. Compile, test and lint libraries before editing TUI or REPL consumers. Migrate
   workspace consumers of breaking experimental API changes without shims.
4. Update contracts and useful examples with the implementation. Compile
   examples through doctests where supported; use `no_run` for examples that
   require a device or network service.
5. Add meaningful tests for changed invariants or behavior. Prove critical new
   regression tests fail when their load-bearing behavior is removed, restore
   the implementation, and verify they pass.
6. Run formatting once at the end of the edit batch, including a touched
   standalone fuzz workspace. Run the complete gate on the finished tree,
   then split that checked work into focused commits.
7. Update affected contracts and explain scoped policy deviations. Stage
   explicit paths and commit with the required scope prefix; keep unpublished
   work local.

## Reviewer checklist

Review code from a caller's perspective and inspect its rendered public
documentation. Apply the complete upstream checklist to a crate review; the
following questions guide ordinary changes:

- Do types and constructors preserve every invariant, including conversion,
  deserialization and mutation paths?
- Are names, ownership, borrowing, traits and conversions consistent with Rust
  conventions and the domain's meaning?
- Do contracts explain inputs, units, bounds, errors and observable effects?
  For I/O, are partial progress, cancellation, deadlines and cleanup explicit?
- Do examples show useful workflows and link related APIs? Can callers use the
  API without consulting implementation source?
- Does the implementation express control flow and state clearly? Are new
  abstractions, clones, shared ownership and formatting exclusions justified?
- Do checks cover the affected crate in isolation, relevant feature combinations
  and platform-specific branches?
- Are deviations scoped, explained and accompanied by remaining verification
  and a reconsideration condition?

Resolve concrete findings during change review. Keep API contracts in rustdoc
and READMEs; record enduring project adaptations in `RUST_STANDARDS.md` with
their scope, rationale and reconsideration condition.
