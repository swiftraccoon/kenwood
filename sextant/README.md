# sextant

Desktop GUI client for D-STAR reflectors (`DExtra` / `DPlus` / `DCS`).
Companion to the POLARIS test reflector — exercises the full
laptop-only `dstar-gateway` + `mbelib-rs` encode/decode pipeline with
no radio in the loop.

**WIP — API, UI, and audio quality will churn. Not intended for
on-air use yet.**

## Scope

- Connect to a reflector (UDP handshake, keepalives, clean disconnect).
- Receive voice: AMBE 3600×2400 frames → PCM → default audio output.
- Transmit voice: default audio input → AMBE 3600×2400 → reflector.
- Linear-interpolation resampling between HW audio rate and 8 kHz AMBE.
- Single window, immediate-mode (egui).

Out of scope: slow-data display, header editing, reflector host-file
browser, recording / playback, device selection UI, high-quality
resampling. File issues or PRs if any of these would be useful.

## Usage

```text
cargo run -p sextant
```

### macOS: microphone permission

Unbundled CLI binaries (like `cargo run`) don't get their own mic
permission prompt — they inherit from the Terminal that launched
them.  If mic capture goes silent and the logs show
`50 consecutive silent TX frames`, macOS has denied access.

**Fastest fix** — grant your terminal permission once:

1. System Settings > Privacy & Security > Microphone
2. Enable the toggle for Terminal / iTerm / whichever shell you use
3. Restart the terminal, rerun `cargo run -p sextant`

**Cleaner fix** — run sextant as a proper `.app` bundle with its own
Info.plist declaring `NSMicrophoneUsageDescription`:

```text
./sextant/macos-bundle.sh         # or --release
open target/Sextant.app
```

On first launch macOS will prompt specifically for Sextant (not
Terminal).  You can revoke/grant later under Privacy & Security.

### End-to-end test against POLARIS

Two terminals — server then client:

```text
# Terminal 1 — start the local reflector
cargo run -p dstar-gateway-server --bin polaris

# Terminal 2 — launch the GUI
cargo run -p sextant
```

In the GUI:

1. Set **Callsign** to your own (≤ 8 ASCII chars, uppercase).
2. Leave **Reflector host / port / callsign** at the defaults
   (`127.0.0.1:30001`, `POLARIS`).
3. Click **Connect**.
4. Click **PTT** to start transmitting (mic audio → AMBE →
   reflector). Click again to stop (EOT is sent).
5. Any other client on the same module hears your audio; anyone
   transmitting on your module plays through your speakers.

A second client is needed to hear yourself — `thd75-repl` can link
to `POLARIS` identically, or run a second `sextant` instance.

## Architecture

```text
GUI thread (egui)           tokio runtime               std thread (cpal)
─────────────────           ─────────────               ─────────────────
App::update()               session::run()              audio::run_audio_worker()
  ├─ draws UI                 ├─ AsyncSession<P>          ├─ cpal input stream
  ├─ sends SessionCommand     ├─ forwards events ◄─────── │   (fills mic ringbuf)
  │   (Connect/Disconnect/    │                            ├─ cpal output stream
  │    StartTx/TxFrame/EndTx) │                            │   (drains speaker ringbuf)
  │   ───────────────────►    ├─ sends voice frames        ├─ AmbeEncoder (TX path)
  ├─ sends AudioCommand ───►  └─ receives voice frames ─►  ├─ AmbeDecoder (RX path)
  │   (StartTx/StopTx/                                      └─ linear resampler
  │    RxFrame)
  └─ drains SessionEvent
```

- Sessions are tokio tasks (the `dstar-gateway` shell). They talk
  UDP to the reflector, decode incoming frames, and forward
  `VoiceRx` to the GUI.
- Audio I/O lives on its own `std::thread` because `cpal::Stream` is
  `!Send` on some platforms. The thread owns both streams, two
  ring buffers (mic → worker, worker → speakers), and the codec
  instances.
- `let _unused = ` on GUI channel sends is intentional: if the
  session task has gone away (shutdown), dropping the send is the
  right thing to do.

## Audio quality caveat

Resampling is linear interpolation — fast, cheap, intelligible for
speech, not bit-accurate. Downsampling from 48 kHz to 8 kHz without
an anti-aliasing filter folds content above 4 kHz back into the
passband. For voice it sounds slightly duller than direct 8 kHz
capture but remains intelligible. A future pass will swap in
`rubato::SincFixedIn` once the end-to-end flow is validated.

## License

GPL-2.0-or-later (base) / GPL-3.0-or-later (through `mbelib-rs`'s
encoder feature). See `LICENSES/` at the workspace root.
