# if-dsp

Sans-io DSP for a 12 kHz low-IF audio stream. Turns the mono 48 kHz
sound-card feed some receivers expose into demodulated SSB/CW/AM audio.

## Scope

- `Channelizer` / `ChannelizerConfig`: the full pipeline that mixes the
  low IF (`IF_CENTER_HZ`) to complex baseband, decimates `INPUT_RATE`
  (48 kHz) to `BASEBAND_RATE` (12 kHz), applies a mode passband,
  demodulates, applies AGC, and interpolates back to `OUTPUT_RATE`
  (48 kHz).
- `DemodMode`: USB, LSB, CW, AM.
- `Agc` / `AgcConfig`: the gain stage, usable standalone.
- `Nco`: numerically controlled oscillator for the IF mix.
- `SpectrumEstimator`: passband spectrum snapshots for tuning displays.

## Sans-io discipline

No I/O, no clocks, no threads. Everything flows through explicit `process`
calls, and steady-state processing never allocates: output buffers are
caller-owned and reused, internal scratch grows once to working size.
Reconfiguration (mode or filter changes) is the documented allocation
exception.

A unit-amplitude IF tone demodulates to approximately unit-amplitude audio
before AGC.

## Consumers

[`thd75-listen`](../thd75-listen/) is the command-line audio-shell consumer,
but its live path is currently blocked before DSP startup by the library's
direct-frequency write quarantine. [`azimuth-core`](../azimuth-core/) wraps the
same channelizer and `SpectrumEstimator` for Azimuth's live iPadOS spectrum,
waterfall, passband, level, clipping, and capture-loss views. Sound-card,
serial, UI, and playback policy remain entirely in those consumers.

## Status

New in July 2026. Pre-release; public API is unstable.

Part of the [kenwood](..) workspace. License: GPL-2.0-or-later.
