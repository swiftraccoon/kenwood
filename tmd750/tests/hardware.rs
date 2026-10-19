//! Read-only checks against a connected TM-D750, run only on request.
//!
//! Set `TMD750_PORT` to the radio's USB serial path and run
//! `cargo test -p kenwood-tmd750 --test hardware -- --ignored`. The suite
//! waits for an operation-panel endpoint to settle, reads the identity and
//! every CAT getter on both bands, reads the fixed MCP probe fragments, and
//! then waits for the identity to return on a fresh connection. It writes no
//! setting and sends no transmit command; the probe's programming session
//! interrupts normal operation until its exit.

use kenwood_schema as _;
use kenwood_transport as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::error::Error as StdError;
use std::sync::atomic::AtomicBool;

use kenwood_tmd750::radio::readiness::{SystemControlHost, await_settled, verify_readiness};
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, discover_serial, open_serial};
use kenwood_tmd750::types::{DstarSlot, MemoryChannelAddress};
use kenwood_tmd750::{Band, Error, McpProbeOutcome, ProtocolError, Radio, RadioModel};
use kenwood_transport::Transport;

type TestResult = Result<(), Box<dyn StdError>>;

/// The enumerated endpoint `TMD750_PORT` names.
fn endpoint() -> Result<SerialCandidate, Box<dyn StdError>> {
    let path = std::env::var("TMD750_PORT")
        .map_err(|_| "set TMD750_PORT to the radio's USB serial path")?;
    discover_serial()?
        .into_iter()
        .find(|candidate| candidate.path == path)
        .ok_or_else(|| format!("{path} is not an enumerated serial endpoint").into())
}

/// Describe a reading, or `NotAvailable`, the radio's answer to a read its
/// current mode does not support.
fn available_or_refused<T: std::fmt::Debug>(result: Result<T, Error>) -> Result<String, Error> {
    match result {
        Ok(value) => Ok(format!("{value:?}")),
        Err(Error::Protocol(ProtocolError::NotAvailable { .. })) => {
            Ok("not available in this mode".to_owned())
        }
        Err(error) => Err(error),
    }
}

/// Every band-indexed CAT getter's typed value for `band`, one labeled line
/// each.
async fn read_band_getters<T: Transport>(
    radio: &mut Radio<T>,
    band: Band,
) -> Result<Vec<String>, Error> {
    Ok(vec![
        format!("{band} mode {:?}", radio.get_operating_mode(band).await?),
        format!("{band} frequency {:?}", radio.get_frequency(band).await?),
        format!("{band} record {:?}", radio.get_channel_record(band).await?),
        format!("{band} power {:?}", radio.get_power_level(band).await?),
        format!("{band} tuning {:?}", radio.get_tuning_mode(band).await?),
        format!("{band} squelch {:?}", radio.get_squelch(band).await?),
        format!("{band} S-meter {:?}", radio.get_smeter(band).await?),
        format!("{band} busy {:?}", radio.get_busy(band).await?),
        format!("{band} attenuator {:?}", radio.get_attenuator(band).await?),
        format!("{band} step {:?}", radio.get_step_size(band).await?),
        format!(
            "{band} memory channel {}",
            available_or_refused(radio.get_current_channel(band).await)?
        ),
    ])
}

/// Every CAT getter's typed value, one labeled line each.
async fn read_every_getter<T: Transport>(radio: &mut Radio<T>) -> Result<Vec<String>, Error> {
    let mut readings = read_band_getters(radio, Band::A).await?;
    readings.extend(read_band_getters(radio, Band::B).await?);
    readings.push(format!("gateway {:?}", radio.get_dv_gateway_mode().await?));
    readings.push(format!(
        "serial {:?}",
        radio.get_serial_information().await?
    ));
    readings.push(format!(
        "power status {:?}",
        radio.get_power_status().await?
    ));
    readings.push(format!("clock {:?}", radio.get_real_time_clock().await?));
    readings.push(format!("AM high cut {:?}", radio.get_am_high_cut().await?));
    readings.push(format!(
        "memory 000 {:?}",
        radio
            .get_memory_channel(MemoryChannelAddress::regular(0)?)
            .await?
    ));
    for slot in 1..=6 {
        let slot = DstarSlot::new(slot)?;
        readings.push(format!(
            "MY {slot} {:?}",
            radio.get_dstar_callsign(slot).await?
        ));
    }
    readings.push(format!(
        "band control {:?}",
        radio.get_band_control().await?
    ));
    readings.push(format!("display {:?}", radio.get_band_display().await?));
    readings.push(format!("MY slot {:?}", radio.get_dstar_slot().await?));
    readings.push(format!(
        "backlight {:?}",
        radio.get_backlight_control().await?
    ));
    readings.push(format!(
        "position {:?}",
        radio.get_my_position_selection().await?
    ));
    readings.push(format!(
        "data rate {:?}",
        radio.get_packet_data_rate().await?
    ));
    readings.push(format!("beacon {:?}", radio.get_beacon_method().await?));
    readings.push(format!(
        "APRS callsign {:?}",
        radio.get_aprs_callsign().await?
    ));
    readings.push(format!("TNC {:?}", radio.get_tnc_mode().await?));
    readings.push(format!("VOX {:?}", radio.get_vox().await?));
    readings.push(format!("VOX delay {:?}", radio.get_vox_delay().await?));
    readings.push(format!("VOX gain {:?}", radio.get_vox_gain().await?));
    readings.push(format!("GPS {:?}", radio.get_gps_settings().await?));
    readings.push(format!("NMEA {:?}", radio.get_gps_sentences().await?));
    readings.push(format!("Bluetooth {:?}", radio.get_bluetooth().await?));
    Ok(readings)
}

#[tokio::test]
#[ignore = "needs a connected TM-D750 named by TMD750_PORT"]
async fn read_only_hardware_suite() -> TestResult {
    let endpoint = endpoint()?;
    let cancelled = AtomicBool::new(false);
    let mut host = SystemControlHost::new();
    let settle = await_settled(&mut host, &endpoint, &cancelled).await;
    assert!(settle.settled(), "the endpoint did not settle: {settle:?}");

    let mut radio = Radio::new(open_serial(&endpoint.path, DEFAULT_BAUD)?);
    let identity = radio.identify().await?;
    assert_eq!(identity.model, RadioModel::TmD750);
    for reading in read_every_getter(&mut radio).await? {
        println!("{reading}");
    }
    let probe = radio.probe_mcp(|| false).await;
    let mut transport = radio.into_transport();
    transport.close().await?;
    assert!(
        matches!(probe.outcome, McpProbeOutcome::AwaitingCatVerification),
        "{:?}",
        probe.outcome
    );
    assert_eq!(probe.identity.as_ref(), Some(&identity));

    let readiness = verify_readiness(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &identity,
        None,
        &cancelled,
    )
    .await;
    assert!(
        readiness.succeeded() && readiness.released(),
        "the identity did not return after the probe's exit: {readiness:?}"
    );
    Ok(())
}
