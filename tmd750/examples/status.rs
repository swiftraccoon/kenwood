//! Print the ordinary CAT state of both bands and the global settings.
//!
//! Opens the selected port with the radio's 9600 baud line settings, proves
//! the identity, then reads each band's frequency, mode, tuning mode, power,
//! squelch, S-meter, busy flag, tuning step and channel record, followed by
//! the global settings with typed reads: band roles, band display, AM high
//! cut, VOX, GPS, Bluetooth, APRS callsign, position source, packet data
//! rate, beacon method, backlight, the selected MY slot and all six MY
//! callsign slots. Nothing is written.
//!
//! Run: `cargo run -p kenwood-tmd750 --example status -- /dev/cu.usbmodem101`

// Dev-dependencies visible to every kenwood-tmd750 example target but unused
// here, acknowledged so `unused_crate_dependencies` stays silent.
use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::transport::{DEFAULT_BAUD, open_serial};
use kenwood_tmd750::types::DstarSlot;
use kenwood_tmd750::{Band, Error, Radio};
use kenwood_transport::{Transport, TransportError};

async fn print_band<T: Transport>(radio: &mut Radio<T>, band: Band) -> Result<(), Error> {
    println!("Band {band}");
    println!("  frequency: {}", radio.get_frequency(band).await?);
    println!("  mode: {}", radio.get_operating_mode(band).await?);
    println!("  tuning: {}", radio.get_tuning_mode(band).await?);
    println!("  power: {}", radio.get_power_level(band).await?);
    println!("  squelch: {}", radio.get_squelch(band).await?);
    println!("  S-meter: {}", radio.get_smeter(band).await?);
    println!(
        "  busy: {}",
        if radio.get_busy(band).await? {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "  attenuator: {}",
        if radio.get_attenuator(band).await? {
            "on"
        } else {
            "off"
        }
    );
    println!("  step: {}", radio.get_step_size(band).await?);
    println!(
        "  channel record: {}",
        radio.get_channel_record(band).await?
    );
    Ok(())
}

async fn print_global<T: Transport>(radio: &mut Radio<T>) -> Result<(), Error> {
    println!("Global");
    println!("  band roles: {}", radio.get_band_control().await?);
    println!("  band display: {}", radio.get_band_display().await?);
    println!("  AM high cut: {}", radio.get_am_high_cut().await?);
    println!("  backlight: {}", radio.get_backlight_control().await?);
    println!("  VOX: {}", radio.get_vox().await?);
    println!("  VOX delay: {}", radio.get_vox_delay().await?);
    println!("  VOX gain: {}", radio.get_vox_gain().await?);
    println!("  GPS: {}", radio.get_gps_settings().await?);
    println!("  NMEA sentences: {}", radio.get_gps_sentences().await?);
    println!(
        "  Bluetooth: {}",
        if radio.get_bluetooth().await? {
            "on"
        } else {
            "off"
        }
    );
    println!("  APRS callsign: {}", radio.get_aprs_callsign().await?);
    println!(
        "  position source: {}",
        radio.get_my_position_selection().await?
    );
    println!(
        "  packet data rate: {}",
        radio.get_packet_data_rate().await?
    );
    println!("  beacon method: {}", radio.get_beacon_method().await?);
    let (tnc, data_band) = radio.get_tnc_mode().await?;
    println!("  TNC: {tnc} on band {data_band}");
    println!("  MY slot: {}", radio.get_dstar_slot().await?);
    for raw in DstarSlot::MIN..=DstarSlot::MAX {
        let entry = radio.get_dstar_callsign(DstarSlot::new(raw)?).await?;
        println!("  callsign {entry}");
    }
    Ok(())
}

async fn read_status<T: Transport>(radio: &mut Radio<T>) -> Result<(), Error> {
    println!("{}", radio.identify().await?);
    println!("DV Gateway: {}", radio.get_dv_gateway_mode().await?);
    print_band(radio, Band::A).await?;
    print_band(radio, Band::B).await?;
    print_global(radio).await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .ok_or("pass the serial port as the first argument")?;
    let mut radio = Radio::new(open_serial(&port, DEFAULT_BAUD)?);
    // Keep the owner even if a query fails; close must still be attempted.
    let operation = read_status(&mut radio).await;
    let mut transport = radio.into_transport();
    let close: Result<(), TransportError> = transport.close().await;
    drop(transport);
    match (operation, close) {
        (Ok(()), Ok(())) => Ok(()),
        (operation, close) => Err(format!(
            "status failed: operation={:?}; close={:?}",
            operation.err(),
            close.err()
        )
        .into()),
    }
}
