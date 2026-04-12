# dstar-gateway-core

Sans-io core for the `dstar-gateway` D-STAR reflector library.

This crate is **runtime-agnostic and I/O-free**. It contains the
wire-format codecs for `DPlus` (REF), `DExtra` (XRF/XLX), and
`DCS` reflector protocols, the typestate session machines for
both client and server, the D-STAR header + voice frame types,
slow-data sub-codec, `DPRS` position parser, host-file parser,
and a layered error hierarchy.

The async (tokio) shell lives in the sibling `dstar-gateway`
crate and the multi-client reflector server lives in
`dstar-gateway-server`. If you're writing your own event loop or
embedding this in a no-tokio environment, depend on this crate
directly and drive it via the [`Driver`] trait.

Alpha. See [the `dstar-gateway` README](../dstar-gateway/README.md)
for the current project status and consumption instructions.

[`Driver`]: https://docs.rs/dstar-gateway-core/*/dstar_gateway_core/session/trait.Driver.html
