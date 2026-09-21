# if-dsp (sans-io)

Pure DSP for the TH-D75 12 kHz IF-over-USB-audio stream. NO I/O, NO
`Instant::now()`, NO steady-state allocation: everything goes through `process`
calls with caller-owned buffers, which the callee clears.

- Filters use rotate-history plus zip-MAC, with no slice indexing anywhere, so the workspace `indexing_slicing` and `unwrap_used` denies hold with zero suppressions in filter code. Keep it that way.
- Amplitude invariant: a unit IF tone produces roughly unit audio (mix x0.5, SSB x2).
- The no-steady-state-allocation rule has one carve-out: `FftPlanner` allocates in `SpectrumEstimator::new`, never inside `process`.
- Consumers must import `if_dsp::Complex32`, the re-export of `rustfft::num_complex::Complex32`, and not their own `num-complex`, or the types will not unify.
- `demod`, `fir` and `resample` have no root re-export: reach them module-qualified.
- All tests are in-module with synthetic signals (tones asserted via Goertzel): no hardware, no fixtures.
