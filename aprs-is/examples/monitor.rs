//! Read-only APRS-IS witness monitor.
//!
//! Connects with a receive-only login and a filter that captures
//! everything relevant to one station: packets from the callsign
//! (`b/`), packets digipeated by it (`d/`), and messages addressed to
//! it (`g/`). Prints each server line with a UTC `HH:MM:SS` label so
//! output can be correlated with radio-side logs.
//!
//! Usage:
//!
//! ```text
//! cargo run -p aprs-is --example monitor -- W1AW
//! cargo run -p aprs-is --example monitor -- W1AW r/35.30/-82.46/50
//! ```
//!
//! The first argument is the base callsign without SSID; further
//! arguments are appended verbatim to the server filter.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aprs_is::{AprsIsClient, AprsIsConfig, AprsIsEvent};

// Examples are separate compilation units that see every crate
// dependency; anchor the unused ones for unused_crate_dependencies.
use aprs as _;
use ax25_codec as _;
use proptest as _;
use thiserror as _;
use tracing as _;

/// Seconds-precision UTC clock label, `HH:MM:SS`.
fn utc_label() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let Some(call) = args.next() else {
        println!("usage: monitor <CALLSIGN> [extra filter terms...]");
        std::process::exit(2);
    };
    let call = call.to_ascii_uppercase();

    let mut filter = format!("b/{call}* d/{call}* g/{call}*");
    for term in args {
        filter.push(' ');
        filter.push_str(&term);
    }

    // Distinct SSID so a same-callsign igate can hold its own APRS-IS
    // session at the same time.
    let mut config = AprsIsConfig::receive_only(&format!("{call}-14"));
    config.filter.clone_from(&filter);

    println!(
        "[{}] connecting as {call}-14, filter: {filter}",
        utc_label()
    );
    let mut client = match AprsIsClient::connect(config).await {
        Ok(c) => c,
        Err(e) => {
            println!("[{}] connect failed: {e}", utc_label());
            std::process::exit(1);
        }
    };

    loop {
        match tokio::time::timeout(Duration::from_secs(20), client.next_event()).await {
            Err(_) => {
                // Idle window elapsed with no server traffic.
                if let Err(e) = client.maybe_send_keepalive().await {
                    println!("[{}] keepalive failed: {e}", utc_label());
                    break;
                }
            }
            Ok(Ok(AprsIsEvent::Packet(pkt))) => {
                println!("[{}] {}", utc_label(), pkt.line);
            }
            Ok(Ok(AprsIsEvent::Comment(line))) => {
                println!("[{}] # {line}", utc_label());
            }
            Ok(Ok(AprsIsEvent::LoggedIn { server })) => match server {
                Some(s) => println!("[{}] logged in to {s}", utc_label()),
                None => println!("[{}] logged in", utc_label()),
            },
            Ok(Ok(AprsIsEvent::LoginRejected { reason })) => {
                println!("[{}] login rejected: {reason}", utc_label());
                break;
            }
            Ok(Ok(AprsIsEvent::Disconnected)) => {
                println!("[{}] server disconnected", utc_label());
                break;
            }
            Ok(Err(e)) => {
                println!("[{}] error: {e}", utc_label());
                break;
            }
        }
    }
}
