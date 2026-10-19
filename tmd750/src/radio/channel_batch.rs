//! Batch memory-channel programming over CAT.

use super::{Progress, Radio};
use crate::error::Error;
use crate::types::{CatMemoryChannelRecord, MemoryChannelAddress};
use kenwood_transport::Transport;

/// One channel's change in a [`Radio::program_memory_channels`] batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelWrite {
    /// Store the record (`ME address,<record>`).
    Store(CatMemoryChannelRecord),
    /// Clear the channel (`ME address,`).
    Clear,
}

/// The channels a batch changed, in order, each with the record the radio
/// read back after storing it, or `None` once cleared.
pub type ProgrammedChannels = Vec<(MemoryChannelAddress, Option<CatMemoryChannelRecord>)>;

/// A batch that stopped at its first failure.
#[derive(Debug, thiserror::Error)]
#[error("memory channel {failed}: {source}")]
pub struct ChannelBatchError {
    /// The channels changed before the failure, as the batch returns them.
    pub completed: ProgrammedChannels,
    /// The channel whose write or clear failed; its state is unknown.
    pub failed: MemoryChannelAddress,
    /// The failure.
    pub source: Error,
}

impl<T: Transport> Radio<T> {
    /// Store or clear each channel over CAT in order, stopping at the first
    /// failure.
    ///
    /// Each store is [`Self::write_memory_channel`] and each clear
    /// [`Self::clear_memory_channel`], so the radio derives every band code;
    /// `progress` counts finished channels. Names take an MCP session:
    /// [`crate::radio::McpSession::write_channel_names`].
    ///
    /// # Errors
    ///
    /// Returns [`ChannelBatchError`] with the channels finished before the
    /// failing one; the remaining channels are not attempted.
    pub async fn program_memory_channels(
        &mut self,
        writes: &[(MemoryChannelAddress, ChannelWrite)],
        mut progress: impl FnMut(Progress),
    ) -> Result<ProgrammedChannels, ChannelBatchError> {
        let mut completed = Vec::with_capacity(writes.len());
        for (address, write) in writes {
            let result = match write {
                ChannelWrite::Store(record) => self
                    .write_memory_channel(*address, record.clone())
                    .await
                    .map(Some),
                ChannelWrite::Clear => self.clear_memory_channel(*address).await.map(|()| None),
            };
            match result {
                Ok(stored) => completed.push((*address, stored)),
                Err(source) => {
                    return Err(ChannelBatchError {
                        completed,
                        failed: *address,
                        source,
                    });
                }
            }
            progress(Progress {
                done: completed.len(),
                total: writes.len(),
            });
        }
        Ok(completed)
    }
}
