# thd75-listen

Experimental accessible SSB/CW/AM demodulator for the Kenwood TH-D75's
IF-over-USB-audio stream. The TH-D75 can present its 12 kHz IF as a USB
sound-card input, and this tool contains the capture, demodulation, playback,
prompt, and state-restoration pipeline. A connected-radio session currently
cannot pass its initial direct-frequency step because unqualified FO/FQ writes
fail closed in [`kenwood-thd75`](../thd75/).

## How it works

- Is designed to tune and configure the radio over the USB CDC serial
  interface (via [`kenwood-thd75`](../thd75/)); direct startup and interactive
  retuning are currently unavailable.
- Captures the 12 kHz IF from the radio's USB audio interface
  (the "ADC stream IN" device).
- Demodulates USB/LSB/CW/AM at baseband and plays the result;
  volume and signal level are adjustable live from the prompt.
- Every radio setting the session may touch is saved first. Exit performs a
  best-effort, read-back-checked restore and reports each failed field;
  frequency restoration currently reports failure for the same FO/FQ safety
  reason.

## Accessibility

Follows the same conventions as [`thd75-repl`](../thd75-repl/): plain
line-oriented prompt output with no cursor-addressed UI, written to work
well with screen readers.

## Usage

```
cargo run -p thd75-listen -- [--port /dev/tty.usbmodem...] [--freq <MHz>]
```

Without `--port` the serial device is auto-detected. The radio must expose its
`ADC stream IN` USB audio interface. Until a direct-frequency writer is
qualified, the command exits at the initial tune and does not start audio.

## Layout

The library (`thd75_listen`) holds pure command parsing, output formatting,
and radio session state; the binary owns all I/O (cpal audio streams,
serial CAT, the terminal).

## Status

New in July 2026. Pre-release; command surface is unstable.

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
