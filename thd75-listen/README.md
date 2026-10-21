# thd75-listen

Experimental accessible SSB/CW/AM demodulator for the Kenwood TH-D75's
IF-over-USB-audio stream. The TH-D75 can present its 12 kHz IF as a USB
sound-card input, and this tool contains the capture, demodulation, playback,
prompt, and state-restoration pipeline. Tuning uses the radio's UP/DW commands
with a frequency readback after every step; direct FO/FQ frequency writes
remain unavailable in [`kenwood-thd75`](https://github.com/swiftraccoon/kenwood/tree/main/thd75).

## How it works

- Tunes and configures the radio over the USB CDC serial interface through
  [`kenwood-thd75`](https://github.com/swiftraccoon/kenwood/tree/main/thd75). Startup and interactive retunes use bounded,
  individually verified UP/DW step walks.
- Captures the 12 kHz IF from the radio's USB audio interface
  (the "ADC stream IN" device).
- Demodulates USB/LSB/CW/AM at baseband and plays the result;
  volume and signal level are adjustable live from the prompt.
- Every radio setting the session may touch is saved first. Exit performs a
  best-effort, read-back-checked restore and reports each failed field;
  the saved tuning step is restored before the original frequency is walked
  back on its original raster.

## Accessibility

Follows the same conventions as [`thd75-repl`](https://github.com/swiftraccoon/kenwood/tree/main/thd75-repl): plain
line-oriented prompt output with no cursor-addressed UI, written to work
well with screen readers.

## Usage

```sh
cargo run -p thd75-listen -- [--port /dev/tty.usbmodem...] [--freq <MHz>]
```

Without `--port` the serial device is auto-detected. The radio must expose its
`ADC stream IN` USB audio interface. A target must be reachable from both the
current frequency and the saved frequency within the bounded step-walk limit.

## Library workflow

The library (`thd75_listen`) validates commands and formats spoken output.
The binary owns radio/session state, audio streams, serial I/O and the terminal.
Command construction uses checked frequency, filter and volume values; the
selected mode imposes a further DSP passband limit before execution.

```rust
use thd75_listen::parser::{Command, FilterWidth, ParseError, SignalLevel, Volume};
use thd75_listen::output::{self, StreamHealth};

let command: Command = "tune 435.640".parse()?;
let Command::Tune(frequency) = command else { unreachable!() };
assert_eq!(output::tuned(frequency), "Tuned to 435.64 megahertz.");
assert!(matches!("tune 435.641".parse::<Command>(), Err(ParseError::OffRaster { .. })));
assert!("status extra".parse::<Command>().is_err());
assert!(Volume::new(101).is_err());
assert!(FilterWidth::new(f32::NAN).is_err());
assert!(SignalLevel::new(f32::INFINITY).is_err());
assert_eq!(
    output::signal(SignalLevel::new(-12.34)?),
    "Signal: 12.3 decibels below full scale."
);
let status = output::status(
    frequency, if_dsp::DemodMode::Usb, FilterWidth::new(2400.0)?,
    Volume::new(60)?, &StreamHealth::default(),
);
assert!(status.contains("Volume: 60 percent"));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Signal readouts reject nonfinite values and clamp finite values to -120 through
zero dBFS. Formatting accepts checked domain values. Text labels and general
error displays are supplied by the caller; they appear verbatim in the output.

On ordinary quit, EOF or a terminal interrupt, the binary stops the audio
streams, joins the DSP worker, then asks `kenwood-thd75` to restore saved radio
settings. Restoration is best effort, with a warning for each failed/skipped
field. Process termination cannot run this asynchronous restoration. Audio
callback failures do not perform I/O or lock state in the callback.

## Status

Pre-release; the API is experimental. `Command` implements `FromStr` with a
typed `ParseError`; numeric payloads and formatting inputs use private validated
types. Consumers replace free command parsing and raw numeric payloads with
those checked constructors. Extra command arguments are rejected.

Part of the [kenwood](https://github.com/swiftraccoon/kenwood) workspace.
License: GPL-2.0-or-later.
