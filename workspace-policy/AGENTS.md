# workspace-policy

Rust developer automation for exact workspace inventory, lint-policy drift and
isolated package checks. Inputs are Cargo manifests and the tracked TOML policy.

## Rules

- Parse TOML with the `toml` crate. Reject unknown configuration fields and
  duplicate inventory or exception entries.
- Root workspace membership is authoritative. Inventory paths are relative,
  portable paths without parent traversal.
- A lint exception identifies one package, group, lint, effective setting and
  reason. Unused exceptions fail the check.
- The CI command checks policy first, runs each package in its own directory and
  stops at the first failing Cargo command. Feature-specific lanes remain in CI.
