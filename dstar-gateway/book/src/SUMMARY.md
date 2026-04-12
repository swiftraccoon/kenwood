# Summary

[Introduction](introduction.md)

# Getting Started

- [What is D-STAR?](getting-started/what-is-dstar.md)
- [Installation & feature flags](getting-started/installation.md)
- [Hello, REF030 (DPlus)](getting-started/hello-dplus.md)
- [Hello, DCS001](getting-started/hello-dcs.md)
- [Hello, XLX307 (DExtra)](getting-started/hello-dextra.md)

# The Type System

- [Why typestate?](typestate/why.md)
- [The Session<P, S> shape](typestate/session.md)
- [Protocol markers and sealing](typestate/protocol-markers.md)
- [State markers and transitions](typestate/state-markers.md)
- [The Failed<S, E> recovery pattern](typestate/failed.md)
- [AnySession for long-lived storage](typestate/any-session.md)
- [Compile-fail tests as documentation](typestate/compile-fail.md)

# The Sans-IO Core

- [Why sans-io?](sans-io/why.md)
- [The Driver protocol](sans-io/driver.md)
- [Time injection](sans-io/time.md)
- [The outbox and timer wheel](sans-io/outbox.md)
- [Writing your own runtime shell](sans-io/custom-shell.md)

# The Wire Format

- [DSVT framing (DPlus + DExtra)](wire/dsvt.md)
- [DCS framing](wire/dcs.md)
- [The DPlus auth protocol](wire/dplus-auth.md)
- [Slow data](wire/slow-data.md)
- [DPRS positions](wire/dprs.md)
- [Reading the constants table](wire/constants.md)

# Errors and Diagnostics

- [The error hierarchy](errors/hierarchy.md)
- [Lenient parsing, strict logging](errors/lenient.md)
- [Writing a custom DiagnosticSink](errors/custom-sink.md)
- [The StrictnessFilter pattern](errors/strict-mode.md)
- [Handling specific error variants](errors/handling.md)

# Building a Reflector

- [The Reflector type](server/reflector.md)
- [Modules and clients](server/modules.md)
- [The fan-out engine](server/fanout.md)
- [Authorization and access policy](server/authorization.md)
- [Cross-protocol forwarding](server/cross-protocol.md)
- [Operating a reflector](server/operating.md)

# Testing

- [Unit and property tests in your code](testing/unit.md)
- [Using the FakeReflector / FakeClient](testing/fakes.md)
- [Conformance pcap replay](testing/conformance.md)
- [Hardware-in-the-loop](testing/hardware.md)

# Cookbook

- [Reconnecting on failure](cookbook/reconnect.md)
- [Bridging two reflectors](cookbook/bridge.md)
- [Logging every diagnostic](cookbook/logging.md)
- [Recording a session for replay](cookbook/recording.md)
- [Building a CLI client (with the blocking shell)](cookbook/cli.md)
- [Embedding in a TUI / REPL](cookbook/tui.md)

# Reference

- [Protocol constants by file/line in ircDDBGateway](reference/ircddbgateway.md)
- [Protocol constants by file/line in xlxd](reference/xlxd.md)
- [Glossary](reference/glossary.md)
- [Bibliography](reference/bibliography.md)
- [Changelog and migration guide](reference/changelog.md)

# Appendix

- [The audit findings that drove this rewrite](appendix/audit.md)
- [Differences from ircDDBGateway behavior](appendix/ircddbgateway-diffs.md)
- [Differences from xlxd behavior](appendix/xlxd-diffs.md)
