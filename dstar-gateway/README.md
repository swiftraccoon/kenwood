# dstar-gateway

D-STAR reflector gateway client library in Rust. Provides async UDP clients for the DExtra (XRF/XLX) and DPlus (REF) reflector protocols.

## Protocols

- **DExtra** (port 30001) — XRF and XLX reflectors
- **DPlus** (port 20001) — REF reflectors, with TCP authentication
- **DCS** (port 30051) — planned

## Architecture

```
[Radio MMDVM] <--your app--> [dstar-gateway] <--UDP--> [Reflector]
```

This crate handles the reflector (network) side. Your application provides the radio (MMDVM) side.

## Features

- D-STAR header encode/decode with CRC-CCITT
- Pi-Star host file parser (2-column and 3-column formats)
- Voice frame types (AMBE + slow data)
- Async UDP clients with automatic keepalives
- Unified `ReflectorClient` enum for protocol-agnostic usage

## References

Protocol formats derived from:
- g4klx/ircDDBGateway (GPL-2.0)
- LX3JL/xlxd (GPL-2.0)
- g4klx/MMDVMHost (GPL-2.0)

## License

GPL-2.0-or-later
