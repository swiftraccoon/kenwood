# Azimuth

Azimuth is a native iPadOS and macOS control center and field guide for the
Kenwood TH-D75.

## Product tour

<p align="center">
  <a href="screenshots/01-radio-remote-control.webp">
    <img src="screenshots/01-radio-remote-control.webp" alt="Azimuth Radio workspace showing the live TH-D75 display and authenticated remote controls" width="100%">
  </a>
  <br>
  <sub>Live TH-D75 color display, all 25 authenticated controls, and negotiated capabilities. Click for the full-size view.</sub>
</p>

<p align="center">
  <strong>Radio</strong> ·
  <a href="screenshots/02-aprs-packet-activity.webp">APRS</a> ·
  <a href="screenshots/03-if-dsp-spectrum-waterfall.webp">IF-DSP</a> ·
  <a href="screenshots/04-settings-catalog.webp">Settings</a> ·
  <a href="screenshots/05-assistant-on-device-automation.webp">Assistant</a> ·
  <a href="screenshots/06-learn-capability-center.webp">Learn</a>
</p>

<details>
<summary><strong>Explore all six workspaces</strong>: compact gallery</summary>
<br>
<table>
  <tr>
    <td width="50%" align="center">
      <a href="screenshots/01-radio-remote-control.webp"><img src="screenshots/01-radio-remote-control.webp" alt="Radio workspace" width="100%"></a><br>
      <strong>Radio</strong><br><sub>Live LCD and authenticated 25-key remote panel.</sub>
    </td>
    <td width="50%" align="center">
      <a href="screenshots/02-aprs-packet-activity.webp"><img src="screenshots/02-aprs-packet-activity.webp" alt="APRS operations workspace" width="100%"></a><br>
      <strong>APRS</strong><br><sub>Host-owned KISS, decoded and raw activity, counters, heard stations, map, and confirmed manual TX.</sub>
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <a href="screenshots/03-if-dsp-spectrum-waterfall.webp"><img src="screenshots/03-if-dsp-spectrum-waterfall.webp" alt="IF-DSP spectrum and waterfall workspace" width="100%"></a><br>
      <strong>IF-DSP</strong><br><sub>Current-frequency physical IF capture, demodulation, spectrum, waterfall, levels, and loss diagnostics.</sub>
    </td>
    <td width="50%" align="center">
      <a href="screenshots/04-settings-catalog.webp"><img src="screenshots/04-settings-catalog.webp" alt="TH-D75 settings catalog" width="100%"></a><br>
      <strong>Settings</strong><br><sub>Complete 400-record catalog, direct menu numbers where defined, 399 live scalar values, and one specialized bitmap.</sub>
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <a href="screenshots/05-assistant-on-device-automation.webp"><img src="screenshots/05-assistant-on-device-automation.webp" alt="On-device Assistant workspace" width="100%"></a><br>
      <strong>Assistant</strong><br><sub>Approval-gated planning with on-device iPadOS dictation.</sub>
    </td>
    <td width="50%" align="center">
      <a href="screenshots/06-learn-capability-center.webp"><img src="screenshots/06-learn-capability-center.webp" alt="Azimuth learning center" width="100%"></a><br>
      <strong>Learn</strong><br><sub>Task-oriented field guides for the complete radio.</sub>
    </td>
  </tr>
</table>
</details>

## Product pillars

- **Radio at a distance.** Show the V1.03.AZM-authenticated 240×180 LCD and
  route every virtual key through the firmware's one-use full-frame guard.
- **APRS operations.** Own the KISS session, configure packet parameters,
  continuously drain and decode received AX.25, and retain counters, a packet
  journal, heard stations, a map, and raw-frame evidence. A blank callsign keeps
  the session receive-only. Message and manual-position transmission are
  explicit, confirmed, one-shot operations; periodic SmartBeaconing and message
  acknowledgement retry/correlation are not implemented.
- **Physical IF analysis.** On iPadOS, save and read-back-verify the radio state,
  reserve Band B, and capture the real 48 kHz mono `ADC stream IN` feed at the
  current VFO frequency. USB, LSB, CW, and AM processing drives the spectrum,
  waterfall, passband, level, clipping, and capture-loss views. Direct
  frequency retuning currently fails closed until a qualified FO/FQ writer is
  available, and demodulated audio playback remains disabled until a safe
  non-radio output route is verified.
- **Every setting, understandable.** Present the complete 400-record MCP
  catalog with search, direct radio menu numbers where defined, official option
  domains, staged diffs, confirmation, stale-value protection, and read-back
  verification. The 399 scalar values use the generic live editor; the power-on
  bitmap remains read-only there until a specialized editor is supplied.
- **Ask Azimuth.** Use Apple's on-device Foundation Models framework to turn
  natural language into a typed, explainable before-and-after plan. The model
  never emits raw CAT bytes or memory offsets. Azimuth validates every item,
  presents Accept and Decline, and automatically performs the complete batch
  through the trusted radio controller only after the operator accepts it.
- **Learn the D75.** Ship an original, searchable capability guide with
  task-oriented walkthroughs and contextual help.
- **USB first.** Use USB-C on M-series iPads through USBDriverKit, and the
  system CDC serial device on macOS.

## Safety contract

The model cannot touch the radio. It proposes catalog setting identifiers and
typed values only. Azimuth validates them against the live catalog and retained
preconditions, presents a concrete diff, and sends nothing unless the operator
explicitly accepts it. Accepted changes use stale-value checks and verified
read-back. RF transmission, reset, and firmware workflows remain outside the
settings planner and require dedicated UI. APRS and IF-DSP temporarily own the
serial session, so CAT-backed screen and settings operations resume only after
their stop-and-requalification path succeeds.

## Development

```bash
../azimuth-core/scripts/build-xcframework.sh
xcodegen generate
open Azimuth.xcodeproj
```

USBDriverKit requires a physical M-series iPad and a TH-D75. The Simulator is
for UI, assistant, catalog, and recorded-transport tests. Live IF capture is
currently iPadOS-only; the macOS build reports its explicit CoreAudio-device
selection blocker instead of analyzing the wrong input.

Lifecycle, recovery, and failure diagnostics are logged by default. Set the
scheme environment variable `AZIMUTH_VERBOSE_USB_TRACE=1` when packet-by-packet
core, transport, and doorbell tracing is needed.
