# Conformance corpus

This directory is a placeholder for the real `dstar-gateway-fuzz-corpus`
submodule which will hold pcap captures of real reflector traffic. Until
the user creates that separate GitHub repo and wires it in as a
submodule, the conformance replay tests in
`dstar-gateway/tests/conformance.rs` will no-op on the missing
corpus and stay green.

When the corpus repo exists, add it with:

```bash
git submodule add https://github.com/<owner>/dstar-gateway-fuzz-corpus \
  dstar-gateway/tests/conformance/corpus
```

The corpus is expected to have one subdirectory per protocol
(`dplus/`, `dextra/`, `dcs/`) containing captured traffic as
`.pcap` files. `tests/conformance.rs` uses `pcap-parser` to strip
the Ethernet/IPv4/UDP headers and feeds the UDP payloads through
each protocol's `decode_server_to_client` and
`decode_client_to_server` entry points — unknown or malformed
packets are expected (reflectors emit plenty) and surface as
diagnostics on a `VecSink`.
