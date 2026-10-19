//! Every stored channel of a backup, listed offline (the programmed memory,
//! program scan, priority and APRS channels, then the CALL and weather
//! channels), and memory channel stores and clears over one MCP session.

mod edit;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use kenwood_tmd750::memory::{ChannelAccess, MemoryImage};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::types::PhysicalChannel;

use super::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

/// Channels subcommand: an offline listing or preview, or one guarded live
/// write.
#[derive(Debug, Parser)]
pub(crate) struct ChannelsRequest {
    #[command(subcommand)]
    command: ChannelsCommand,
}

#[derive(Debug, Subcommand)]
enum ChannelsCommand {
    /// Print every stored channel of a backup; never writes.
    List(ListRequest),
    /// Show the channels a set of stores and clears changes; never writes.
    Preview(edit::EditSelection),
    /// Write the stores and clears in one session, comparing and reading back
    /// each page.
    Apply(edit::ApplyRequest),
}

#[derive(Debug, Args)]
struct ListRequest {
    /// Successful standard configuration-backup report.json.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
}

impl ChannelsRequest {
    pub(super) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if matches!(self.command, ChannelsCommand::Apply(_)) && !explicit_port {
            return Err(CommandError(
                "mcp channels apply requires an explicit --port before mcp".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) fn run_offline(request: &ChannelsRequest) -> Option<AppResult<()>> {
    match &request.command {
        ChannelsCommand::List(list) => Some(print_list(list)),
        ChannelsCommand::Preview(edits) => Some(edit::preview(edits)),
        ChannelsCommand::Apply(_) => None,
    }
}

pub(super) async fn run_selected(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &ChannelsRequest,
) -> AppResult<()> {
    match &request.command {
        ChannelsCommand::Apply(apply) => edit::run_apply(endpoint, baud, apply).await,
        ChannelsCommand::List(list) => print_list(list),
        ChannelsCommand::Preview(edits) => edit::preview(edits),
    }
}

fn print_list(list: &ListRequest) -> AppResult<()> {
    let snapshot = Snapshot::load(&list.backup)?;
    for line in lines(covered(&snapshot)?) {
        output::line(format_args!("{line}"));
    }
    Ok(())
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
                if index.index() < PhysicalChannel::FIRST_PROGRAM_SCAN {
                    lines.push(format!("{entry}, group {}", entry.flag().group()));
                } else {
                    lines.push(entry.to_string());
                }
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
