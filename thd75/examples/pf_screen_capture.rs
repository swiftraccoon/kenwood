//! Hardware-verification probe: assign front-panel PF1 to "Screen Capture".
//!
//! Writes the Screen Capture function byte to the front-panel PF1
//! assignment slot in MCP memory, then leaves the radio for a manual
//! key-press test.
//!
//! Offsets and value were established by differential MCP dump:
//!   - PF-key assignment block: MCP 0x107A..=0x107E (front PF1/2, mic PF1/2/3)
//!   - "Screen Capture" function byte: 0x1F
//!
//! This is an *off-menu* assignment — the radio's Menu 940 never offers
//! Screen Capture for a front-panel key. The probe exists to test whether
//! the firmware dispatch honours it regardless.
//!
//! Usage:
//! ```text
//! cargo run -p kenwood-thd75 --example pf_screen_capture -- /dev/cu.usbmodemXXXX
//! ```
//!
//! Programming mode resets the USB connection on exit. After it runs:
//!   1. Press the front-panel [PF1] key.
//!   2. Look in /KENWOOD/TH-D75/CAPTURE/ on the microSD card for a new .bmp.
//!   3. Restore PF1 via Menu 940.

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
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

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;

/// MCP page holding the PF-key assignment block (0x107A is in page 0x10).
const PF_KEY_PAGE: u16 = 0x10;
/// Front-panel PF1 assignment, as an offset within page 0x10 (0x107A & 0xFF).
const FRONT_PF1: usize = 0x7A;
/// Screen Capture function byte (hardware-verified).
const SCREEN_CAPTURE: u8 = 0x1F;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port} ...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    println!("Writing front-panel PF1 (MCP 0x107A) = 0x{SCREEN_CAPTURE:02X} ...");
    println!("Radio will show 'PROG MCP'; the USB connection resets on exit.\n");

    let mut previous = 0_u8;
    radio
        .modify_memory_page(PF_KEY_PAGE, |page| {
            previous = page[FRONT_PF1];
            page[FRONT_PF1] = SCREEN_CAPTURE;
        })
        .await?;

    println!("front PF1: 0x{previous:02X} -> 0x{SCREEN_CAPTURE:02X}  (expected 0x1D -> 0x1F)");
    println!();
    println!("Now, on the radio:");
    println!("  1. Press the front-panel [PF1] key.");
    println!("  2. Check /KENWOOD/TH-D75/CAPTURE/ on the microSD for a new .bmp.");
    println!("  3. Restore PF1 via Menu 940 when done.");

    Ok(())
}
