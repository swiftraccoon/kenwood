# sextant

Desktop GUI client for D-STAR reflectors (`DExtra` / `DPlus` / `DCS`).
Companion to the POLARIS test reflector — exercises the full
laptop-only `dstar-gateway` + `mbelib-rs` encode/decode pipeline with
no radio in the loop.

**WIP — protocol details and audio quality still churn. Fine for
listening; treat transmit as experimental.**

## Features

- Connect to DExtra / DPlus / DCS reflectors: searchable directory
  (XLX registry + DPlus host list), favorites, recent-connection
  shortcuts, auto-reconnect with backoff.
- Receive voice: AMBE decode with packet-loss concealment, playback
  priming against network jitter, click-free stream boundaries, and
  per-stream loss statistics.
- Transmit voice: PTT button or spacebar, mic level metering, TX
  silence test, transmit-from-WAV.
- Slow data both ways: text messages and GPS (DPRS / NMEA) decoded
  and displayed; operator text + GPS beacon on transmit.
- Heard-station list (optionally persisted), event log, link-health
  readout (reflector last-heard age).
- Audio device selection, RX recording to WAV, local WAV playback.
- Windowed-sinc resampling between the hardware rate and the 8 kHz
  codec rate (anti-aliasing built in).

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
  │   (StartTx/StopTx/                                      └─ sinc resampler
  │    RxFrame/RxLost/RxEnd)
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

## License

GPL-2.0-or-later (base) / GPL-3.0-or-later (through `mbelib-rs`'s
encoder feature). See `LICENSES/` at the workspace root.
