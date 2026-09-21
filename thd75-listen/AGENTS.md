# thd75-listen

Accessible headless IF demodulator shell over the radio's USB audio capture stream.

## Commands

```bash
cargo run -p thd75-listen -- --freq 162.550 --port /dev/cu.usbmodem101
echo "status" | cargo run -p thd75-listen 2>&1 | grep callback   # harness form, never `> log`
```

- The binary takes only `--port <device>` and `--freq <MHz>`, parsed by hand in `src/main.rs` with no clap. `--freq` goes through the same `tune` grammar, so an off-step value is refused at startup.
- Headless-harness gotcha: with stdin from a pipe AND stdout redirected to a FILE (no tty anywhere), macOS throttles the process and audio callbacks stall after about 3 buffers. Piping stdout to another process keeps audio alive. Real terminal sessions are unaffected. The invocation is the variable; rustyline, cpal topology, device caching and serial coexistence are not.
- `status` reports input and output callback counters: 3-and-frozen means a throttled process, advancing means the pipeline is alive. Check it first when audio is silent.

## Rules

- No `tokio::signal` anywhere. Ctrl-C arrives as a rustyline `Interrupted` and takes the quit and restore path, so process SIGINT semantics stay untouched.
- Audio callbacks take no locks and no unwraps. Chunks cross threads through bounded `sync_channel`s, volume and signal level cross as `AtomicU32` f32 bits, and overrun/underrun are counters plus a one-shot announce flag; never spam the prompt.
- Radio state discipline is library-owned: `Radio::enter_if_tap` guards Band-B VFO mode, saves every touched setting including the tuning step, and proves IF engagement; `Radio::restore_if_tap` restores in the hardware-required order. Do not reimplement either here.
- `tune` accepts 5 kHz multiples (exact integer parse, no float rounding) and retunes through `retune_if_tap`; off-step or over-1000-step targets are refused with guidance. The library walk verifies every step before sending the next, so never add a shell-side step burst.
- The IF tap only carries signal from the VHF/UHF front end, so satellite, 2 m, and 70 cm are the working envelope and anything below the VHF boundary is hiss only. Ear test: 162.550 in USB mode is a continuous NOAA warble.
- Output strings are lint-tested through `thd75_repl::lint`, a dev-dependency, so an accessibility-rule change in that crate breaks these tests and only under `--all-targets`. One self-contained spoken line each.
