# aprs-is

APRS-IS (APRS Internet Service) TCP client, checked server filters, and
byte-preserving TNC2 packet envelopes. The Tokio client owns sockets and time;
information-field decoding lives in
[`aprs`](https://github.com/swiftraccoon/kenwood/tree/main/aprs).

## Scope

- `AprsIsClient`: connect (with retry), login, 120 s keepalive, next-event loop, text packet sending, byte-preserving RF uplink sending, and shutdown.
- `AprsIsConfig`: checked login identity, authentication, endpoint, software words and filter. Named builders reject invalid settings and complete login lines above 512 bytes before a connection opens.
- `aprs_is_passcode(callsign: &str) -> i32`: the standard two-byte-XOR hash.
- `build_login_string(&AprsIsConfig) -> Result<String, AprsIsError>`: `user <call> pass <code> vers <sw> <ver> [filter <spec>]` (enforces the APRS-IS callsign/SSID grammar, one-word software fields, and a filter without `CR`/`LF`).
- `AprsIsFilter`: typed geographic, distance, pattern, packet-type, symbol and Q-construct clauses, including `q//I`. `to_wire()` allocates one clause; `join()` accepts an iterator and checks spatial counts and strict-object placement. `raw()` checks one extension clause's framing; `with_raw_filter()` accepts a complete ASCII expression whose server-specific grammar remains unchecked.
- `QConstruct`: classification of the ten Q-construct tokens. `format_is_packet_with_qconstruct` appends an explicit construct to a checked text envelope. `igate_format_for_is` applies RF gating opt-outs and loop checks while preserving binary information bytes and terminal TNC delimiter handling.
- `AprsIsUplinkLine`: validated byte-native wire line with exactly one trailing CRLF, no embedded CR/LF, and a 512-byte maximum including framing. `AprsIsClient::send_uplink_line` accepts this proof type; `send_raw_bytes` validates existing wire bytes before sending them unchanged.
- `AprsIsLine`: strict, byte-preserving TNC2 packet parser. Source,
  destination, and route elements use APRS-IS-native alphanumeric identity
  types (including one- or two-character SSIDs and nine-byte server logins);
  Q-constructs have their own enum variant; embedded line framing and malformed
  identities are rejected with `AprsIsLineError`.
- `parse_is_line`: lightweight server-line classifier for the socket event
  loop (comments/keepalives versus packet candidates).
- `format_is_packet`: text-only outbound builder returning a validated `AprsIsUplinkLine`. Use `igate_format_for_is` for RF-to-IS path rewriting and byte-native information fields.

## Scope boundaries

APRS information-field decoding is in `aprs`. This crate validates
the APRS-IS/TNC2 envelope without interpreting the information bytes. The
strict parser retains those bytes exactly, including non-UTF-8 Mic-E data, so a
caller can pass them to `aprs` or construct a byte-exact third-party RF frame.

## Status

Pre-release. Public API is unstable. Connection recovery is explicit through
`AprsIsClient::reconnect()` or `connect_with_retry()`. The latter accepts a
nonzero attempt limit or `None` for unlimited attempts, retries only transport
failures, and uses delays from one to sixty seconds.

## Checked configuration and filters

```rust
use aprs::{Latitude, Longitude};
use aprs_is::{AprsIsConfig, AprsIsFilter, FilterDistance, FilterPatterns};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let range = AprsIsFilter::Range {
    lat: Latitude::new(35.25)?,
    lon: Longitude::new(-97.75)?,
    distance_km: FilterDistance::new(100.0)?,
};
let objects = AprsIsFilter::StrictObject(FilterPatterns::new(["FIRE*", "EMS*"])?);
let config = AprsIsConfig::receive_only("N0CALL-TS")?
    .with_software("station-monitor", "1.0")?
    .with_filter([&range, &objects])?;
assert_eq!(config.filter(), "r/35.25/-97.75/100 os/FIRE*/EMS*");
# Ok(())
# }
```

## Packet bytes and identities

```rust
use aprs_is::{AprsIsAddressField, AprsIsIdentity, AprsIsLine, AprsIsUplinkLine};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let identity = AprsIsIdentity::new("AE5PL-TS", AprsIsAddressField::Source)?;
let packet = AprsIsLine::parse(b"AE5PL-TS>APRS,TCPIP*:hello")?;
assert_eq!(packet.source(), &identity);
assert_eq!(packet.information(), b"hello");
let line = AprsIsUplinkLine::from_body_bytes(b"AE5PL-TS>APRS,TCPIP*:hello".to_vec())?;
assert!(line.as_bytes().ends_with(b"\r\n"));
let owned: Vec<u8> = line.into();
assert_eq!(owned, b"AE5PL-TS>APRS,TCPIP*:hello\r\n");
# Ok(())
# }
```

`AprsIsPacket::raw()` borrows received bytes. `line()` derives a lossy UTF-8
display view, and `into_bytes()` transfers the original allocation. RF gating
uses the raw bytes and the opt-out/loop checks in `igate_format_for_is`.

## Deadlines and cancellation

TCP establishment and login writing share `CONNECT_TIMEOUT` (ten seconds).
Login acknowledgement arrives through `next_event()`, which matches the
response grammar and this connection's identity. An unverified response is
accepted for receive-only authentication and rejected for passcode authentication.
`login_state()` remains `LoginState::Pending` until that response. Packet uploads
require `LoginState::Verified`; other states return
`AprsIsError::UnverifiedConnection` before writing. Comments remain available
for keepalives. RF gating selects `IgateMessaging::Bidirectional` for `qAR`
or `IgateMessaging::ReceiveOnly` for `qAO` according to RF return-message
capability. Both require a verified Internet connection.

`next_event()` has no intrinsic deadline. Cancelling it retains the incomplete
line and oversized-line drain state. The inbound buffer is capped at 1024 bytes;
an oversized line yields `AprsIsError::ReadLineTooLong` after its newline or EOF.
Subsequent calls resume at the next line.

An outgoing write and flush share `SEND_TIMEOUT` (ten seconds). Cancellation,
timeout, or I/O failure retires the outgoing stream. Further sends return
`AprsIsError::ConnectionRetired` until reconnection succeeds. Reconnect preserves
the old client until replacement connection and login writing complete.
`shutdown()` consumes the client, bounds shutdown by ten seconds, and drops both
socket halves on cancellation. The optional 120-second keepalive timer is a
client policy; callers invoke it explicitly.

## References

- APRS-IS: <https://www.aprs-is.net/>
- Q-construct rules: <https://www.aprs-is.net/q.aspx>

Part of the [kenwood](https://github.com/swiftraccoon/kenwood) workspace.
License: GPL-2.0-or-later.
