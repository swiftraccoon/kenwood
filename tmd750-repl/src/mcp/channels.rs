//! Every stored channel of a backup, listed offline: the programmed memory,
//! program scan, priority and APRS channels, then the CALL and weather
//! channels.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{ChannelAccess, MemoryImage};
use kenwood_tmd750::types::PhysicalChannel;

use super::snapshot::Snapshot;
use crate::{AppResult, output};

/// Channels subcommand: an offline listing.
#[derive(Debug, Parser)]
pub(crate) struct ChannelsRequest {
    #[command(subcommand)]
    command: ChannelsCommand,
}

#[derive(Debug, Subcommand)]
enum ChannelsCommand {
    /// Print every stored channel of a backup; never writes.
    List(ListRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
}

pub(super) fn run(request: &ChannelsRequest) -> AppResult<()> {
    match &request.command {
        ChannelsCommand::List(list) => {
            let snapshot = Snapshot::load(&list.backup)?;
            for line in lines(covered(&snapshot)?) {
                output::line(format_args!("{line}"));
            }
            Ok(())
        }
    }
}

/// The backup's image, after checking it captured the flag, record and name
/// tables of every channel.
pub(super) fn covered(snapshot: &Snapshot) -> AppResult<&MemoryImage> {
    let [flags, records, names] = ChannelAccess::regions()?;
    let _flags = snapshot.image_covering(flags)?;
    let _records = snapshot.image_covering(records)?;
    snapshot.image_covering(names)
}

/// One line per stored channel and per CALL and weather channel; a slot
/// whose bytes do not decode is listed with the reason.
fn lines(image: &MemoryImage) -> Vec<String> {
    let access = ChannelAccess::from_memory_image(image);
    let mut lines = Vec::new();
    let mut stored = 0_usize;
    for index in PhysicalChannel::all() {
        match access.entry(index) {
            Ok(entry) if entry.channel().is_none() => {}
            Ok(entry) => {
                stored += 1;
                lines.push(entry.to_string());
            }
            Err(error) => lines.push(format!("{index}: undecodable: {error}")),
        }
    }
    lines.push(format!("Stored memory channels: {stored}."));
    for (channel, entry) in kenwood_tmd750::memory::CallChannel::ALL
        .into_iter()
        .zip(access.call_channels())
    {
        lines.push(entry.map_or_else(
            |error| format!("{channel}: undecodable: {error}"),
            |entry| entry.to_string(),
        ));
    }
    for (channel, entry) in
        kenwood_tmd750::memory::WeatherChannel::all().zip(access.weather_channels())
    {
        lines.push(entry.map_or_else(
            |error| format!("{channel}: undecodable: {error}"),
            |entry| entry.to_string(),
        ));
    }
    lines
}

#[cfg(test)]
pub(super) mod tests;
