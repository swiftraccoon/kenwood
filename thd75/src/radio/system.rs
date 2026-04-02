//! System-level radio methods: battery level, beep, lock, dual-band, frequency step, bluetooth, attenuator, auto-info.

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::Band;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get beep setting (BE read).
    ///
    /// D75 RE: `BE x` (x: 0=off, 1=on).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_beep(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading beep setting");
        let response = self.execute(Command::GetBeep).await?;
        match response {
            Response::Beep { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Beep".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set beep on/off (BE write).
    ///
    /// D75 RE: `BE x` (x: 0=off, 1=on).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_beep(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::debug!(enabled, "setting beep");
        let response = self.execute(Command::SetBeep { enabled }).await?;
        match response {
            Response::Beep { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Beep".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the battery charge level (BL read).
    ///
    /// Returns 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green).
    /// Read-only — the radio does not accept BL writes.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_battery_level(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading battery level");
        let response = self.execute(Command::GetBatteryLevel).await?;
        match response {
            Response::BatteryLevel { level } => Ok(level),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BatteryLevel".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the key lock state (LC read).
    ///
    /// On the TH-D75, LC controls the key lock. The CAT value is inverted
    /// relative to the radio's display: `LC 0` means locked, `LC 1` means
    /// unlocked. The MCP offset for the lock setting is `0x1060`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_lock(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading key lock state");
        let response = self.execute(Command::GetLock).await?;
        match response {
            Response::Lock { locked } => Ok(locked),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Lock".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the key lock state (LC write).
    ///
    /// See [`get_lock`](Self::get_lock) for details on the CAT value
    /// inversion and the corresponding MCP offset.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_lock(&mut self, locked: bool) -> Result<(), Error> {
        tracing::info!(locked, "setting key lock");
        let response = self.execute(Command::SetLock { locked }).await?;
        match response {
            Response::Lock { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Lock".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the dual-band enabled state (DL read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_dual_band(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading dual-band state");
        let response = self.execute(Command::GetDualBand).await?;
        match response {
            Response::DualBand { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DualBand".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the dual-band enabled state (DL write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_dual_band(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::debug!(enabled, "setting dual-band state");
        let response = self.execute(Command::SetDualBand { enabled }).await?;
        match response {
            Response::DualBand { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DualBand".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Step frequency down on the given band (DW action).
    ///
    /// Per KI4LAX CAT reference: DW tunes the current band's frequency
    /// down by the current step size. Counterpart to UP (frequency up).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn frequency_down(&mut self, band: Band) -> Result<(), Error> {
        tracing::debug!(?band, "stepping frequency down");
        let response = self.execute(Command::FrequencyDown { band }).await?;
        // The radio echoes either `DW\r` (parsed as FrequencyDown) or a bare
        // OK depending on firmware version and AI mode state.
        match response {
            Response::FrequencyDown | Response::Ok => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "FrequencyDown".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the Bluetooth enabled state (BT read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_bluetooth(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading Bluetooth state");
        let response = self.execute(Command::GetBluetooth).await?;
        match response {
            Response::Bluetooth { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Bluetooth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the Bluetooth enabled state (BT write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_bluetooth(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::info!(enabled, "setting Bluetooth state");
        let response = self.execute(Command::SetBluetooth { enabled }).await?;
        match response {
            Response::Bluetooth { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Bluetooth".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the attenuator state for the given band (RA read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_attenuator(&mut self, band: Band) -> Result<bool, Error> {
        tracing::debug!(?band, "reading attenuator state");
        let response = self.execute(Command::GetAttenuator { band }).await?;
        match response {
            Response::Attenuator { enabled, .. } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Attenuator".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the attenuator state for the given band (RA write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_attenuator(&mut self, band: Band, enabled: bool) -> Result<(), Error> {
        tracing::debug!(?band, enabled, "setting attenuator state");
        let response = self
            .execute(Command::SetAttenuator { band, enabled })
            .await?;
        match response {
            Response::Attenuator { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Attenuator".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the auto-info mode (AI write). This is a write-only command.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_auto_info(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::info!(enabled, "setting auto-info mode");
        let response = self.execute(Command::SetAutoInfo { enabled }).await?;
        match response {
            Response::AutoInfo { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AutoInfo".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the radio type/region code (TY read).
    ///
    /// Returns a tuple of (region code, variant number). For example,
    /// `("K", 2)` indicates a US-region radio, hardware variant 2.
    ///
    /// This command is not in the firmware's 53-command dispatch table.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_radio_type(&mut self) -> Result<(String, u8), Error> {
        tracing::debug!("reading radio type/region");
        let response = self.execute(Command::GetRadioType).await?;
        match response {
            Response::RadioType { region, variant } => Ok((region, variant)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RadioType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the USB Out select state (IO read).
    ///
    /// Per KI4LAX CAT reference and Operating Tips §5.10.5:
    /// 0 = AF (audio frequency output), 1 = IF (12 kHz centered IF signal
    /// for SSB/CW/AM, 15 kHz bandwidth), 2 = Detect (pre-detection signal).
    ///
    /// Menu 102 (USB Out Select) controls this. IF/Detect output is only
    /// available when in Single Band mode on Band B.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_io_port(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading I/O port state");
        let response = self.execute(Command::GetIoPort).await?;
        match response {
            Response::IoPort { value } => Ok(value),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "IoPort".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the USB Out select state (IO write).
    ///
    /// See [`get_io_port`](Self::get_io_port) for value meanings.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_io_port(&mut self, value: u8) -> Result<(), Error> {
        tracing::debug!(value, "setting I/O port state");
        let response = self.execute(Command::SetIoPort { value }).await?;
        match response {
            Response::IoPort { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "IoPort".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Query SD card / programming interface status (SD read).
    ///
    /// The firmware's SD handler primarily checks for `SD PROGRAM` to enter
    /// MCP programming mode. The bare `SD` read response indicates
    /// programming interface readiness, not SD card presence.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_sd_status(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading SD/programming interface status");
        let response = self.execute(Command::GetSdCard).await?;
        match response {
            Response::SdCard { present } => Ok(present),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "SdCard".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get MCP status (0E read).
    ///
    /// Returns `N` (not available) in normal operating mode. This mnemonic
    /// appears to be MCP-related.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_mcp_status(&mut self) -> Result<String, Error> {
        tracing::debug!("reading MCP status");
        let response = self.execute(Command::GetMcpStatus).await?;
        match response {
            Response::McpStatus { value } => Ok(value),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "McpStatus".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    // -----------------------------------------------------------------------
    // MCP-based setting writes (for settings where CAT writes are rejected)
    //
    // The TH-D75 firmware rejects CAT write commands for several settings
    // (BE, BL, DW return `?` for all write formats). These methods bypass
    // CAT entirely and write directly to the verified MCP memory offsets.
    //
    // Each method enters MCP programming mode, reads the containing page,
    // modifies the target byte, writes the page back, and exits. The USB
    // connection does not survive the MCP transition — after calling any of
    // these methods, drop the Radio and reconnect.
    // -----------------------------------------------------------------------

    /// Set key beep on/off via MCP memory write.
    ///
    /// The CAT `BE` command is a firmware stub on the TH-D75 — it always
    /// returns `?` for writes. This method writes directly to the verified
    /// MCP offset (`0x1071`) instead.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. The USB connection drops after
    /// exit. The `Radio` instance should be dropped and a fresh connection
    /// established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading the page,
    /// writing the page, or exiting programming mode fails.
    pub async fn set_beep_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        const OFFSET: usize = 0x1071;
        // 0x1071 / 256 = 0x10 = 16, fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        const PAGE: u16 = (OFFSET / 256) as u16;
        const BYTE_INDEX: usize = OFFSET % 256;

        tracing::info!(enabled, offset = OFFSET, "setting key beep via MCP");
        self.modify_memory_page(PAGE, |data| {
            data[BYTE_INDEX] = u8::from(enabled);
        })
        .await
    }

    /// Set VOX enabled on/off via MCP memory write.
    ///
    /// Writes directly to the verified MCP offset (`0x101B`). This
    /// provides an alternative to CAT for modes where CAT writes are
    /// rejected.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. The USB connection drops after
    /// exit. The `Radio` instance should be dropped and a fresh connection
    /// established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading the page,
    /// writing the page, or exiting programming mode fails.
    pub async fn set_vox_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        const OFFSET: usize = 0x101B;
        // 0x101B / 256 = 0x10 = 16, fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        const PAGE: u16 = (OFFSET / 256) as u16;
        const BYTE_INDEX: usize = OFFSET % 256;

        tracing::info!(enabled, offset = OFFSET, "setting VOX enable via MCP");
        self.modify_memory_page(PAGE, |data| {
            data[BYTE_INDEX] = u8::from(enabled);
        })
        .await
    }

    /// Set lock on/off via MCP memory write.
    ///
    /// Writes directly to the verified MCP offset (`0x1060`). This
    /// provides an alternative to CAT for modes where CAT writes are
    /// rejected.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. The USB connection drops after
    /// exit. The `Radio` instance should be dropped and a fresh connection
    /// established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading the page,
    /// writing the page, or exiting programming mode fails.
    pub async fn set_lock_via_mcp(&mut self, locked: bool) -> Result<(), Error> {
        const OFFSET: usize = 0x1060;
        // 0x1060 / 256 = 0x10 = 16, fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        const PAGE: u16 = (OFFSET / 256) as u16;
        const BYTE_INDEX: usize = OFFSET % 256;

        tracing::info!(locked, offset = OFFSET, "setting lock via MCP");
        self.modify_memory_page(PAGE, |data| {
            data[BYTE_INDEX] = u8::from(locked);
        })
        .await
    }

    /// Set Bluetooth on/off via MCP memory write.
    ///
    /// Writes directly to the verified MCP offset (`0x1078`). This
    /// provides an alternative to CAT for modes where CAT writes are
    /// rejected.
    ///
    /// # Connection lifetime
    ///
    /// This enters MCP programming mode. The USB connection drops after
    /// exit. The `Radio` instance should be dropped and a fresh connection
    /// established for subsequent CAT commands.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading the page,
    /// writing the page, or exiting programming mode fails.
    pub async fn set_bluetooth_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        const OFFSET: usize = 0x1078;
        // 0x1078 / 256 = 0x10 = 16, fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        const PAGE: u16 = (OFFSET / 256) as u16;
        const BYTE_INDEX: usize = OFFSET % 256;

        tracing::info!(enabled, offset = OFFSET, "setting Bluetooth via MCP");
        self.modify_memory_page(PAGE, |data| {
            data[BYTE_INDEX] = u8::from(enabled);
        })
        .await
    }
}
