# aprs

APRS information-field codecs, checked outgoing reports, messaging, relay
policy, beacon scheduling and heard-station storage. The crate performs no I/O
and reads no clock. Callers supply monotonic `Instant` values for expiry and
scheduling, retain returned work through transport cancellation, and record
transmission completion after a successful write.

## Packet construction and parsing

`AprsFrameHeader` owns the source, selected software identity and relay path.
Its consuming methods transfer those values into an `Ax25Packet`; corresponding
methods without `_packet` produce a KISS data frame. Report bodies borrow checked
text when immediate encoding suffices. `ObjectReport`, `ItemReport` and
`MiceReport` name their fields and retain owned text without copying it.

```rust
use aprs::{AprsData, AprsFrameHeader, AprsSymbol, Latitude, Longitude,
    PositionReportText, Tocall};
use ax25_codec::{Ax25Address, DigipeaterPath};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let source: Ax25Address = "N0CALL-7".parse()?;
let packet = AprsFrameHeader::new(source, Tocall::new("APTEST")?, DigipeaterPath::empty())
    .position_report_packet(Latitude::new(49.058333)?, Longitude::new(-72.029167)?,
        AprsSymbol::HOUSE, &PositionReportText::new("Station")?);
let AprsData::Position(position) = AprsData::from_bytes(packet.information())? else {
    return Err("expected a position".into());
};
assert!((position.latitude().as_degrees() - 49.058333).abs() < 0.00001);
assert_eq!(position.comment(), "Station");
# Ok(())
# }
```

`AprsData::from_bytes_with_destination` also decodes Mic-E, whose latitude and
message selector occupy the destination address. The ordinary parser returns
`AprsError::MicERequiresDestination` for that format. `MiceReport::new` rejects
longitudes that round to 180 degrees because the Mic-E degree table ends at 179.
Compressed observations retain motion, radio range or altitude in named checked
variants. DAO observations preserve datum identity and encoded precision.

Named object reports and Mic-E reports retain the data needed for encoding:

```rust
use aprs::{AprsData, AprsFrameHeader, AprsReportTimestamp, AprsSymbol, Course,
    Latitude, Longitude, MiceReport, MiceSpeed, MiceStatusText, ObjectName,
    ObjectReport, PositionReport, PositionReportText, ReportState, Tocall};
use ax25_codec::DigipeaterPath;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let report = ObjectReport::new(ObjectName::new("WX SITE")?, ReportState::Live,
    AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?,
    PositionReport::new(Latitude::new(49.0)?, Longitude::new(-72.0)?,
        AprsSymbol::HOUSE, PositionReportText::new("Weather station")?));
let packet = AprsFrameHeader::new("N0CALL".parse()?, Tocall::new("APTEST")?,
    DigipeaterPath::empty()).object_packet(&report);
let AprsData::Object(object) = AprsData::from_bytes(packet.information())? else {
    return Err("expected an object".into());
};
assert_eq!(object.name().as_str(), "WX SITE");

let report = MiceReport::new(Latitude::new(49.0)?, Longitude::new(-72.0)?,
    MiceSpeed::new(20)?, Course::new(90)?, AprsSymbol::CAR,
    MiceStatusText::new("Mobile")?)?;
let packet = report.to_packet("N0CALL-7".parse()?, DigipeaterPath::empty());
let data = AprsData::from_bytes_with_destination(packet.information(),
    packet.header.destination.callsign.as_str())?;
assert!(matches!(data, AprsData::Position(_)));
# Ok(())
# }
```

## Messages and transmission state

`DirectMessageAddressee` and `BulletinAddressee` enforce destination-dependent
text grammar. Direct text reserves `{` for an ID trailer; bulletin text retains
it literally. `MessageControl` distinguishes acknowledgement from rejection and
retains a checked `MessageId`. Received messages and their IDs are immutable.

```rust
use aprs::{AprsFrameHeader, AprsMessenger, DirectMessageAddressee, MessageText, Tocall};
use ax25_codec::DigipeaterPath;
use std::time::Instant;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# let write_frame = |_frame: &[u8]| -> std::io::Result<()> { Ok(()) };
let now = Instant::now(); // The application supplies time to the protocol core.
let header = AprsFrameHeader::new("N0CALL-7".parse()?, Tocall::new("APTEST")?,
    DigipeaterPath::empty());
let mut messenger = AprsMessenger::new(header);
let id = messenger.queue_message(&DirectMessageAddressee::new("W1AW-3")?,
    &MessageText::new("Hello")?)?;
if let Some((pending_id, frame)) = messenger.peek_frame_to_send(now) {
    assert_eq!(pending_id, id);
    write_frame(&frame)?;
    let attempt = messenger.commit_send(&pending_id, now)?;
    assert_eq!(attempt.get(), 1);
}
# Ok(())
# }
```

Queue and incoming-history capacities are explicit host policy. Full queues
return typed capacity errors. Cancellation before `commit_send` leaves the
attempt eligible. Acknowledgements match complete sender and recipient
addresses, including SSIDs. Exhausted messages remain pending for one retry
interval after the final recorded attempt, allowing its acknowledgement to
arrive. Clock reversal retains pending and deduplication state.

Receive compatibility preserves malformed ID trailers as literal printable
text. `PARM.` and `UNIT.` accept the 68-byte chapter-13 body limit; ordinary
messages and bulletins use 67 bytes. Weather bulletins retain their
variable-width chapter-14 bodies.

## Weather and telemetry

Weather measurements name their units and enforce wire-field ranges. Missing
measurements remain absent; construction never clamps them into valid-looking
values. Telemetry reports contain exactly five checked analog readings and an
explicit eight-channel `TelemetryBits` value.

```rust
use aprs::{AprsFrameHeader, AprsPositionlessWeatherReport, AprsTelemetry,
    AprsWeather, AprsWeatherTimestamp, Fahrenheit, TelemetryAnalogValue,
    TelemetryBits, TelemetrySequence, Tocall};
use ax25_codec::DigipeaterPath;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let mut weather = AprsWeather::new();
weather.set_temperature(Some(Fahrenheit::new(72)?));
let timestamp = AprsWeatherTimestamp::month_day_hour_minute_utc(10, 9, 23, 45)?;
let report = AprsPositionlessWeatherReport::new(timestamp, weather);
let packet = AprsFrameHeader::new("N0CALL".parse()?, Tocall::new("APTEST")?,
    DigipeaterPath::empty()).weather_packet(&report);
let decoded = AprsPositionlessWeatherReport::from_bytes(packet.information())?;
assert_eq!(decoded.timestamp(), timestamp);
let reading = TelemetryAnalogValue::new(123)?;
let telemetry = AprsTelemetry::new(TelemetrySequence::new(42)?, [reading; 5],
    TelemetryBits::from_bits(0b10101010), None);
assert_eq!(telemetry.digital().to_string(), "10101010");
# Ok(())
# }
```

`TelemetryLabels` retains an exact checked prefix of channel names or units.
`TelemetryEquationCoefficients` retains up to fifteen finite coefficients,
including a partial final triple. `TelemetryEquation` names a complete triple.
The established three-digit analog-value extension accepts values through 999;
APRS 1.0.1 defines values through 255. This extension is described individually
rather than asserting blanket APRS 1.2 conformance.

## Relay decisions

`DigipeaterAliases` names the three alias mechanisms. `DigipeaterLimits` bounds
retained duplicate identities and delayed relays; exhaustion returns a typed
`DigiAction::CapacityExceeded`. A returned relay changes protocol decision state;
the caller retains it until transport writing succeeds or explicitly abandons it.

```rust
use aprs::{AprsFrameHeader, AprsSymbol, DigiAction, DigipeaterAliases,
    DigipeaterConfig, Latitude, Longitude, PositionReportText, Tocall};
use ax25_codec::{DigipeaterPath, RouteEntry};
use std::time::{Duration, Instant};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let packet = AprsFrameHeader::new("N0CALL".parse()?, Tocall::new("APTEST")?,
    DigipeaterPath::new(vec![RouteEntry::new("WIDE1", 1)?])?)
    .position_report_packet(Latitude::EQUATOR, Longitude::PRIME_MERIDIAN,
        AprsSymbol::HOUSE, &PositionReportText::default());
let aliases = DigipeaterAliases {
    uidigipeat: vec!["WIDE1-1".parse()?],
    ..DigipeaterAliases::default()
};
let mut relay = DigipeaterConfig::new("N1GATE".parse()?, aliases);
relay.set_viscous_delay(Duration::ZERO);
let now = Instant::now();
assert!(matches!(relay.process(&packet, now), DigiAction::Relay { .. }));
assert!(matches!(relay.process(&packet, now), DigiAction::Duplicate));
# Ok(())
# }
```

## Beacon scheduling and station storage

`SmartBeaconingConfig` validates the supplied thresholds and formulas. Its
defaults are application policy. `SmartBeaconing` selects work from supplied
speed, heading and time; `beacon_sent_with` records a completed transmission.

```rust
use aprs::{SmartBeaconing, SmartBeaconingConfig, Speed};
use std::time::Instant;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let now = Instant::now();
let stopped = Speed::from_kmh(0.0)?;
let mut beacon = SmartBeaconing::new(SmartBeaconingConfig::default());
assert!(beacon.should_beacon(stopped, None, now));
beacon.beacon_sent_with(stopped, None, now);
assert!(!beacon.should_beacon(stopped, None, now));
# Ok(())
# }
```

`StationList` has a nonzero capacity and supplied-time expiry. It keys observations
by complete `Ax25Address`, provides allocation-free borrowed iteration, and
retains last-heard time across a clock reversal.

```rust
use aprs::{AprsData, StationList};
use ax25_codec::{Ax25Address, DigipeaterPath};
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let source: Ax25Address = "W1AW-3".parse()?;
let data = AprsData::from_bytes(b">Monitoring")?;
let mut stations = StationList::new(NonZeroUsize::new(16).ok_or("capacity")?,
    Duration::from_secs(600));
stations.update(&source, &data, &DigipeaterPath::empty(), Instant::now());
assert_eq!(stations.iter().count(), 1);
assert_eq!(stations.get(&source).ok_or("station missing")?.address(), &source);
# Ok(())
# }
```

## Scope and references

AX.25 framing belongs to
[`ax25-codec`](https://github.com/swiftraccoon/kenwood/tree/main/ax25-codec), KISS
to [`kiss-tnc`](https://github.com/swiftraccoon/kenwood/tree/main/kiss-tnc), and
Internet sockets and connection time to
[`aprs-is`](https://github.com/swiftraccoon/kenwood/tree/main/aprs-is).

Protocol contracts follow [APRS 1.0.1](https://www.aprs.org/doc/APRS101.PDF),
individually supported [extensions](https://www.aprs.org/aprs12.html), and the
[DAO specification](https://www.aprs.org/datum.txt). Public API remains
experimental and unpublished. License: GPL-2.0-or-later.
