//! Identify the connected TM-D750 and print its power, clock and mode state.
//!
//! Lists the serial ports the library recognizes, opens the selected one with
//! the radio's 9600 baud line settings, proves the identity with `ID`, `FV`
//! and `TY`, reads the power status, serial information, clock, DV Gateway
//! state and both band modes, then closes. Nothing is written.
//!
//! Run: `cargo run -p kenwood-tmd750 --example identify`
//!
//! Pass a port to skip discovery:
//! `cargo run -p kenwood-tmd750 --example identify -- /dev/cu.usbmodem101`

// Dev-dependencies visible to every kenwood-tmd750 example target but unused
// here, acknowledged so `unused_crate_dependencies` stays silent.
use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::transport::{DEFAULT_BAUD, discover_serial, open_serial};
use kenwood_tmd750::types::{RealTimeClock, SerialInformation};
use kenwood_tmd750::{Band, DvGatewayMode, Error, Identity, OperatingMode, Radio};
use kenwood_transport::{Transport, TransportError};

/// Everything the example reads on one connection.
struct Snapshot {
    identity: Identity,
    power_on: bool,
    serial: SerialInformation,
    clock: RealTimeClock,
    gateway: DvGatewayMode,
    band_a: OperatingMode,
    band_b: OperatingMode,
}

/// The first recognized TM-D750 endpoint, or an error listing what was found.
fn select_port() -> Result<String, Box<dyn std::error::Error>> {
    let candidates = discover_serial()?;
    if let Some(candidate) = candidates.iter().find(|candidate| candidate.is_tmd750()) {
        return Ok(candidate.path.clone());
    }
    let listed: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    Err(
        format!("no TM-D750 USB endpoint among {listed:?}; pass the port as the first argument")
            .into(),
    )
}

async fn read_snapshot<T: Transport>(radio: &mut Radio<T>) -> Result<Snapshot, Error> {
    Ok(Snapshot {
        identity: radio.identify().await?,
        power_on: radio.get_power_status().await?,
        serial: radio.get_serial_information().await?,
        clock: radio.get_real_time_clock().await?,
        gateway: radio.get_dv_gateway_mode().await?,
        band_a: radio.get_operating_mode(Band::A).await?,
        band_b: radio.get_operating_mode(Band::B).await?,
    })
}

fn report(
    operation: Result<Snapshot, Error>,
    close: Result<(), TransportError>,
) -> Result<(), Box<dyn std::error::Error>> {
    match (operation, close) {
        (Ok(snapshot), Ok(())) => {
            println!("{}", snapshot.identity);
            println!("Power: {}", if snapshot.power_on { "on" } else { "off" });
            println!("Serial: {}", snapshot.serial);
            println!("Clock: {}", snapshot.clock);
            println!("DV Gateway: {}", snapshot.gateway);
            println!(
                "Band A mode: {}; Band B mode: {}",
                snapshot.band_a, snapshot.band_b
            );
            Ok(())
        }
        (operation, close) => Err(format!(
            "identify failed: operation={:?}; close={:?}",
            operation.err(),
            close.err()
        )
        .into()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = match std::env::args().nth(1) {
        Some(port) => port,
        None => select_port()?,
    };
    println!("Opening {port} at {DEFAULT_BAUD} baud");
    let mut radio = Radio::new(open_serial(&port, DEFAULT_BAUD)?);
    // Keep the owner even if a query fails; close must still be attempted.
    let operation = read_snapshot(&mut radio).await;
    let mut transport = radio.into_transport();
    let close = transport.close().await;
    drop(transport);
    report(operation, close)
}
