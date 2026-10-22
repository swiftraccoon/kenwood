# mmdvm-core (sans-io)

MMDVM wire codec: frames, command bytes, status, version and NAK parsing. No
I/O, no async, no clock. `mmdvm` is its tokio shell, and `lodestar-core` uses
it directly. The layering rule is in the root `AGENTS.md` section
`Workspace boundaries`.

## Rules

- `MMDVMHost` (`Modem.cpp`, `Modem.h`) is the reference byte for byte; a divergence is a bug. Cite it de-pathed per the root `AGENTS.md` section `Reference hierarchy`, and keep each file's derived-work attribution header.
- `ModemMode::from_byte` is lossy by design: an unknown byte decodes as `Idle`, pinned by `unknown_byte_maps_to_idle`. Callers that must not read an unknown mode as idle use the strict parse in `mmdvm::probe`; never change the core fallback to suit one caller.
- `MmdvmFrame` owns at most 506 payload bytes; its fields stay private. Both wire forms decode incrementally, and encoding selects the short form through 252 bytes. Command-specific TX restrictions stay in the shell.
- `VersionResponse` accepts supported versions 1 and 2 only. Protocol 2 requires the full twenty-byte fixed header before description bytes. Description text is checked as UTF-8; the core never substitutes replacement characters.
- The only `SetConfig` payload sent today is the D-STAR-only one built in `mmdvm::dstar`; the core exposes no provisional configuration structure.
- Status parsing returns flags without emitting diagnostics; the caller owns logging. `StatusFlags::from_bits_truncate` explicitly discards the unassigned top bit.
- Status layouts depend on the protocol version (`MIN_V1_LEN` 7 bytes, `MIN_V2_LEN` 12); a short payload is `MmdvmError::InvalidStatusLength`, never a padded parse.
- Command constants and `ModemMode` cover DMR, YSF, P25, NXDN, POCSAG and FM, but only D-STAR is exercised end to end; treat the others as unverified against a modem.
