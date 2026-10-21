# workspace-policy

Checks Rust workspace membership and lint requirements against
`.config/rust-policy.toml`.

```sh
cargo run -p workspace-policy -- check
cargo run -p workspace-policy -- ci
```

All commands accept `--root <directory>` after the command. The default root is
the current directory. Paths in the policy file are relative to that root and
contain only ASCII letters, numbers, underscores, hyphens, dots and slashes;
absolute paths and parent traversal are rejected.

`check` requires an exact inventory of root workspace members and standalone
fuzz manifests. Each fuzz workspace must restate the complete root Rust, Clippy
and rustdoc lint tables without changes, and its package must inherit them.
It compares each workspace member's effective Rust, Clippy and rustdoc
lint tables with the workspace tables. Every difference needs one documented
exception, including a missing lint. Exceptions with no matching difference fail
the check. Every manifest that restates lint tables requires the
`KNOWN DIVERGENCE FROM WORKSPACE` comment.

The root policy must forbid `unsafe_code` and deny Clippy's `all`, `pedantic`,
`nursery` and `cargo` groups. Every member must retain those four Clippy groups
at `deny` or `forbid`; a documented exception cannot weaken or omit them.
Stronger settings still require an exact documented difference. Native-code
exceptions to member `unsafe_code` remain subject to the same explicit policy
and separate source allowlist checks.

`ci` performs the checks, then runs
`cargo clippy --all-targets -- -D warnings` and `cargo test` sequentially in
each package directory. It stops and returns failure on the first failed
command. Cargo is selected from the `CARGO` environment variable when present,
or from `PATH`. CI retains separate platform and feature-specific checks.

The policy file uses `version = 1`, `[[packages]]` records with
`name` and `path`, `[[fuzz]]` records with `manifest`, and
`[[deviations]]` records with `package`, `group`, `lint`, `level` and `reason`.
An exception's level is the effective lint setting, or `omitted`. Lint priority
is checked together with its level; nonzero priority is written as
`<level>:<priority>`. No exception changes a manifest's lint settings.

GPL-2.0-or-later.
