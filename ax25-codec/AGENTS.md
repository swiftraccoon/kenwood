# ax25-codec

`#![no_std]` plus `alloc`, sans-io, zero async or I/O. Leaf crate: no workspace
path dependencies. The layering rule lives in the root `AGENTS.md` section
`Workspace boundaries`.

## Rules

- The H-bit lives on `RouteEntry`, never on `Ax25Address`. An address carries no path bits.
- `DigipeaterPath` is the validated newtype `aprs` consumes in public signatures; add entries through `try_push` / `try_insert` rather than building a bare list.
- UI frames are fully supported for APRS. I and S frame control decoding is present, but the connected-mode state machine is out of scope: do not grow one here.
- Third-party wire-format cross-check material is kept out of the repository. Never cite it from committed content; keep such notes in `CLAUDE.local.md` per the root `AGENTS.md` section `Reference hierarchy`.

## no_std test code

Test modules sit inside `#![no_std]`, so the workspace conventions in the root
`AGENTS.md` section `Test code` need core and
alloc paths instead of `std::`:

- `type TestResult = Result<(), Box<dyn core::error::Error>>;`
- `fn to_test_err<E: core::fmt::Debug>(e: E) -> TestCaseError { TestCaseError::fail(alloc::format!("{e:?}")) }`
- `use alloc::boxed::Box;` and `use alloc::vec;` explicitly; there is no prelude for them.

Copy the existing helper in `src/frame.rs` rather than reaching for `std::`.
