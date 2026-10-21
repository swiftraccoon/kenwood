# if-dsp (sans-io)

Pure DSP for the TH-D75 12 kHz IF-over-USB-audio stream. NO I/O, NO
`Instant::now()`. Processing reuses buffers; `process` clears caller-owned output.
Channelizer scratch and output vectors may grow for a larger block. FIR sample
steps and AGC allocate nothing. Spectrum feeding allocates only at construction.

- Filters use rotate-history plus zip-MAC, with no slice indexing. Never add expectations for `indexing_slicing` or `unwrap_used` in filter code. Numeric cast expectations state the kernel's rounding or precision contract.
- SSB amplitude invariant: a unit IF tone produces roughly unit audio before AGC (mix x0.5, SSB x2). AM output instead follows carrier level and modulation depth.
- `SpectrumEstimator` retains FFT scratch from `get_inplace_scratch_len` and uses `process_with_scratch`; never switch to the allocating `Fft::process` convenience method. Pending input stays within one segment regardless of feed size.
- Rates, frequencies, widths, tap counts, rate factors and FFT sizes enter through validated types. Config fields remain private, and mode/filter replacement validates and builds before mutating stream state.
- Consumers must import `if_dsp::Complex32`, the re-export of `rustfft::num_complex::Complex32`, and not their own `num-complex`, or the types will not unify.
- `demod`, `fir` and `resample` have no root re-export: reach them module-qualified.
- All tests are in-module with synthetic signals (tones asserted via Goertzel): no hardware, no fixtures.
