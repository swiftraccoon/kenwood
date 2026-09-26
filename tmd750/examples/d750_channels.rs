//! List the programmed memory channels of a `.d750` configuration file.
//!
//! Parses the file with `parse_d750`, borrows the channel tables of its image
//! and prints every slot whose flag marks it programmed: the physical index,
//! the name and the decoded record. Both file layouts cover the channel
//! tables. No radio is opened.
//!
//! Run: `cargo run -p kenwood-tmd750 --example d750_channels -- radio.d750`

// Dev-dependencies visible to every kenwood-tmd750 example target but unused
// here, acknowledged so `unused_crate_dependencies` stays silent.
use kenwood_schema as _;
use kenwood_transport as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::memory::ChannelAccess;
use kenwood_tmd750::{FileLayout, parse_d750};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("pass the .d750 file path as the first argument")?;
    let config = parse_d750(&std::fs::read(&path)?)?;
    let layout = match config.layout() {
        FileLayout::Full => "full image",
        FileLayout::WithoutStartupScreen => "image without the startup screen",
    };
    println!(
        "{path}: {layout}, {} image bytes",
        config.image_bytes().len()
    );
    let channels = ChannelAccess::new(config.image_bytes())?;
    let mut programmed = 0usize;
    for entry in channels.programmed() {
        println!("{}", entry?);
        programmed += 1;
    }
    println!("{programmed} programmed channels");
    Ok(())
}
