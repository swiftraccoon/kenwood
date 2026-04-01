//! System-level radio methods: backlight, beep, lock, dual-band, dual watch, bluetooth, attenuator, auto-info.

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

    /// Get the backlight brightness level (BL read).
    ///
    /// D75 RE: `BL x` (x: brightness level).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_backlight(&mut self) -> Result<u8, Error> {
        tracing::debug!("reading backlight brightness");
        let response = self.execute(Command::GetBacklight).await?;
        match response {
            Response::Backlight { level } => Ok(level),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Backlight".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the backlight brightness level (BL write).
    ///
    /// D75 RE: `BL x` (x: brightness level).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_backlight(&mut self, level: u8) -> Result<(), Error> {
        tracing::debug!(level, "setting backlight brightness");
        let response = self.execute(Command::SetBacklight { level }).await?;
        match response {
            Response::Backlight { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Backlight".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the lock/backlight control state (LC read).
    ///
    /// Note: On the TH-D75, LC may control display backlight rather than
    /// key lock (the `LC` command controls backlight on this model).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_lock(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading lock/backlight state");
        let response = self.execute(Command::GetLock).await?;
        match response {
            Response::Lock { locked } => Ok(locked),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Lock".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the lock/backlight control state (LC write).
    ///
    /// Note: On the TH-D75, LC may control display backlight rather than
    /// key lock. See [`get_lock`](Self::get_lock) for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_lock(&mut self, locked: bool) -> Result<(), Error> {
        tracing::info!(locked, "setting lock/backlight");
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

    /// Get the dual watch enabled state (DW read).
    ///
    /// D75 RE: `DW x` (x: 0=off, 1=on).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_dual_watch(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading dual watch state");
        let response = self.execute(Command::GetDualWatch).await?;
        match response {
            Response::DualWatch { enabled } => Ok(enabled),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DualWatch".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the dual watch enabled state (DW write).
    ///
    /// D75 RE: `DW x` (x: 0=off, 1=on).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_dual_watch(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::debug!(enabled, "setting dual watch state");
        let response = self.execute(Command::SetDualWatch { enabled }).await?;
        match response {
            Response::DualWatch { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DualWatch".into(),
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

    /// Get the I/O port state (IO read).
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

    /// Set the I/O port state (IO write).
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
