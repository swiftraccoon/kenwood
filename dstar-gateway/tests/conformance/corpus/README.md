# Conformance corpus

This directory holds pcap captures of reflector traffic for the
conformance replay tests in `dstar-gateway/tests/conformance.rs`. The
captures are not committed; drop local `.pcap` files here and run the
tests with `--ignored`. On an empty corpus the tests no-op and stay
green.

The corpus is expected to have one subdirectory per protocol
(`dplus/`, `dextra/`, `dcs/`) containing captured traffic as
`.pcap` files. `tests/conformance.rs` uses `pcap-parser` to strip
the Ethernet/IPv4/UDP headers and feeds the UDP payloads through
each protocol's `decode_server_to_client` and
`decode_client_to_server` entry points. Unknown or malformed
packets are expected (reflectors emit plenty) and surface as
diagnostics on a `VecSink`.
