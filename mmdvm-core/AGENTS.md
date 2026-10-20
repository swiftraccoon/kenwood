# mmdvm-core (sans-io)

MMDVM wire codec: frames, command bytes, status, version and NAK parsing. No
I/O, no async, no clock. `mmdvm` is its tokio shell, and `lodestar-core` uses
it directly. The layering rule is in the root `AGENTS.md` section
`Workspace boundaries`.

## Rules

- `MMDVMHost` (`Modem.cpp`, `Modem.h`) is the reference byte for byte; a divergence is a bug. Cite it de-pathed per the root `AGENTS.md` section `Reference hierarchy`, and keep each file's derived-work attribution header.
- `ModemMode::from_byte` is lossy by design: an unknown byte decodes as `Idle`, pinned by `unknown_byte_maps_to_idle`. Callers that must not read an unknown mode as idle use the strict parse in `mmdvm::probe`; never change the core fallback to suit one caller.
- `ModemConfig` is a stub with no wire encoder. The only `SetConfig` payload sent today is the D-STAR-only one built in `mmdvm::dstar`.
- Status layouts depend on the protocol version (`MIN_V1_LEN` 7 bytes, `MIN_V2_LEN` 12); a short payload is `MmdvmError::InvalidStatusLength`, never a padded parse.
- Command constants and `ModemMode` cover DMR, YSF, P25, NXDN, POCSAG and FM, but only D-STAR is exercised end to end; treat the others as unverified against a modem.
