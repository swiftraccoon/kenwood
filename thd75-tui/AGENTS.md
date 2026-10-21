# thd75-tui

Terminal dashboard with one retained radio task and one joined keyboard reader.

- Model libraries must compile, pass their tests and be lint-clean before
  changing this consumer.
- Never poll SM or BY directly. Apply pushed BY through `StateMonitor` and read
  SM only for its `BecameBusy` transition. New streams start with unavailable
  meter/busy values.
- Missing CAT observations stay `None`. A relative edit must refuse an
  unavailable current value; never choose an Off, zero or first-enum fallback.
- Keep the proven CAT serial across USB re-enumeration. Close the previous
  connection before opening candidates; identify each candidate with read-only
  ID/AE before AI or writes. Native Bluetooth retains the exact opened address.
  A failed close stops replacement. Never send a recovery preamble to an
  unproved rediscovered radio.
- Retain and join the keyboard and radio tasks on every ordinary exit. Stop an
  active APRS client, retain its returned radio after failure, attempt CAT
  restoration and observe connection close before dropping the runtime.
- Stage a typed MCP setting on a private candidate. Compare the entire expected
  live page before writing and commit the local snapshot only after verified
  completion. Keep the prior image and require refresh after uncertain writes.
  Do not restore blind full-image or raw-byte write commands.
- The Kenwood MMDVM wire profile is unsupported. Preserve the pure
  `UnsupportedProfile` refusal before Terminal entry or modem writes; never
  substitute a generic firmware profile. D-STAR CAT preparation remains usable.
- Use model `ALL`, `MIN`, `MAX` and checked constructors for setting domains.
  Unsupported stored values must remain visible or return an error, never be
  coerced into a menu value.
- Direct frequency and persisted per-channel editing remain unavailable while
  their model writers are quarantined. Do not claim a prepared reflector URCALL
  was transmitted, a queued APRS message was sent, or an unknown FM state is Off.
- Hardware-free tests use injected terminal operations, keyboard polling,
  scripted transports and temporary cache directories. No test may open a live
  terminal, native Bluetooth connection or radio.
