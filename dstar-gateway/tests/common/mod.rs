//! Shared test helpers for the tokio shell loopback suites.

#![expect(
    dead_code,
    reason = "each integration test uses a subset of the helper surface"
)]

pub mod fake_reflector;
