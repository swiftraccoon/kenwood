# thd75-listen

Accessible SSB/CW/AM demodulator for the Kenwood TH-D75's IF-over-USB-audio
stream. The TH-D75 can present its 12 kHz IF as a USB sound-card input;
this tool tunes the radio over CAT, demodulates that stream with
[`if-dsp`](../if-dsp/), and plays the audio on the default output device,
adding listening modes the radio itself does not demodulate.

## How it works

- Tunes and configures the radio over the USB CDC serial interface
  (via [`kenwood-thd75`](../thd75/)).
- Captures the 12 kHz IF from the radio's USB audio interface
  (the "ADC stream IN" device).
- Demodulates USB/LSB/CW/AM at baseband and plays the result;
  volume and signal level are adjustable live from the prompt.
- Every radio setting the session touches is saved first and restored on
  every exit path, so the radio comes back exactly as it was.

## Accessibility

Follows the same conventions as [`thd75-repl`](../thd75-repl/): plain
line-oriented prompt output with no cursor-addressed UI, written to work
well with screen readers.

## Usage

```
cargo run -p thd75-listen -- [--port /dev/tty.usbmodem...] [--freq <MHz>]
```

Without `--port` the serial device is auto-detected. The radio must have
its IF output enabled for the selected band.

## Layout

The library (`thd75_listen`) holds pure command parsing, output formatting,
and radio session state; the binary owns all I/O (cpal audio streams,
serial CAT, the terminal).

## Status

New in July 2026. Pre-release; command surface is unstable.

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
