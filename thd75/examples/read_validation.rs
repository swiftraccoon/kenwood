//! Validate lossless typed CAT reads against a connected TH-D75.
//!
//! This example is deliberately read-only. It issues only `FV`, `TY`, `FQ`,
//! `FO`, `MR`, `ME`, and `RT` queries and prints every transport chunk so the
//! typed results can be compared with the exact wire responses.
//!
//! Run:
//! `cargo run -p kenwood-thd75 --example read_validation -- /dev/cu.usbmodem1234`

// Dependencies visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without weakening
// the lint configuration.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::future::Future;

use kenwood_thd75::error::TransportError;
use kenwood_thd75::transport::{SerialTransport, Transport};
use kenwood_thd75::types::{Band, MemorySelector};
use kenwood_thd75::{Error, Radio};

#[derive(Debug)]
struct WireTrace<T> {
    inner: T,
}

impl<T> WireTrace<T> {
    const fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: Transport> Transport for WireTrace<T> {
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), TransportError>> + Send {
        println!("TX {:?}", String::from_utf8_lossy(data));
        self.inner.write(data)
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.inner.read(buffer).await?;
        if let Some(received) = buffer.get(..count)
            && !received.is_empty()
        {
            println!("RX {:?}", String::from_utf8_lossy(received));
        }
        Ok(count)
    }

    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.close()
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.inner.set_baud_rate(baud)
    }

    fn reopen(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.inner.reopen()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());
    println!("Opening {port} at 115200 baud");
    let transport = SerialTransport::open(&port, SerialTransport::DEFAULT_BAUD)?;
    let mut radio = Radio::connect(WireTrace::new(transport)).await?;

    let firmware = radio.get_firmware_version().await?;
    println!("typed FV => {firmware:?}");

    let radio_type = radio.get_radio_type().await?;
    println!("typed TY => {radio_type:?}");

    let mut selectors = Vec::<MemorySelector>::new();
    for band in [Band::A, Band::B] {
        let band_number = u8::from(band);

        let frequency = radio.get_frequency(band).await?;
        println!(
            "typed FQ {band_number} => frequency_hz={:010}",
            frequency.as_hz()
        );

        let record = radio.get_frequency_full(band).await?;
        println!("typed FO {band_number} => {record:#?}");

        match radio.get_current_channel(band).await {
            Ok(selector) => {
                println!("typed MR {band_number} => {selector}");
                if !selectors.contains(&selector) {
                    selectors.push(selector);
                }
            }
            Err(Error::NotAvailable) => {
                println!("typed MR {band_number} => NotAvailable (band is not in memory mode)");
            }
            Err(error) => return Err(error.into()),
        }
    }

    if selectors.is_empty() {
        println!("typed ME => skipped (MR returned no active memory selectors)");
    }
    for selector in selectors {
        match radio.read_memory(selector).await {
            Ok(record) => println!("typed ME {selector} => {record:#?}"),
            Err(Error::NotAvailable) => println!("typed ME {selector} => NotAvailable"),
            Err(error) => return Err(error.into()),
        }
    }

    let clock = radio.get_real_time_clock().await?;
    println!(
        "typed RT => {clock:?}, wire_payload={:?}",
        clock.to_wire_string()
    );

    radio.disconnect().await?;
    println!("Disconnected");
    Ok(())
}
