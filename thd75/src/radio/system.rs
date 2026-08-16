//! System-level radio methods: identity, clock, battery level, beep,
//! backlight, band presentation, Bluetooth, attenuator, and auto-info.
//!
//! The `set_*_via_mcp` methods write registry-verified MCP cells for
//! settings whose CAT write is rejected or stubbed (beep, beep volume,
//! VOX, Bluetooth).

use crate::error::{Error, ProtocolError};
use crate::protocol::programming;
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{
    BacklightControl, Band, BandMode, BatteryLevel, LinkedVolumeLevel, RadioClock, RadioType,
    SerialInformation, UsbAudioOutput,
};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Get the real-time clock (RT bare read).
    ///
    /// Hardware-verified: bare `RT\r` returns either a calendar-valid
    /// `RT YYMMDDHHmmss` value or the exact `RT ------------` unavailable
    /// sentinel.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_real_time_clock(&mut self) -> Result<RadioClock, Error> {
        tracing::debug!("reading real-time clock");
        let response = self.execute(Command::GetRealTimeClock).await?;
        match response {
            Response::RealTimeClock { clock } => Ok(clock),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RealTimeClock".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read the radio's serial number and opaque model code (AE read).
    ///
    /// Although the mnemonic begins with `A`, `AE` is an identity query, not
    /// an APRS operation. The returned [`SerialInformation`] retains the exact
    /// eight-byte serial number and three-byte model code as validated types.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_serial_information(&mut self) -> Result<SerialInformation, Error> {
        tracing::debug!("reading radio serial information");
        let response = self.execute(Command::GetSerialInfo).await?;
        match response {
            Response::SerialInformation(information) => Ok(information),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "SerialInformation".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the battery charge level (BL read).
    ///
    /// Returns 0=Empty (Red), 1=1/3 (Yellow), 2=2/3 (Green), 3=Full (Green),
    /// 4=Charging (USB power connected). Read-only.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use kenwood_thd75::types::BatteryLevel;
    /// use kenwood_thd75::{MockTransport, Radio};
    ///
    /// let mut mock = MockTransport::new();
    /// mock.expect(b"BL\r", b"BL 3\r");
    ///
    /// let mut radio = Radio::new(mock);
    /// assert_eq!(radio.get_battery_level().await?, BatteryLevel::Full);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_battery_level(&mut self) -> Result<BatteryLevel, Error> {
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

    /// Get the LCD backlight control mode (LC read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_backlight_control(&mut self) -> Result<BacklightControl, Error> {
        tracing::debug!("reading backlight control mode");
        let response = self.execute(Command::GetBacklightControl).await?;
        match response {
            Response::BacklightControl { mode } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BacklightControl".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the LCD backlight control mode (LC write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_backlight_control(&mut self, mode: BacklightControl) -> Result<(), Error> {
        tracing::info!(?mode, "setting backlight control mode");
        let response = self.execute(Command::SetBacklightControl { mode }).await?;
        match response {
            Response::BacklightControl { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BacklightControl".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the single-band or dual-band selection (DL read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_band_mode(&mut self) -> Result<BandMode, Error> {
        tracing::debug!("reading band presentation");
        let response = self.execute(Command::GetBandMode).await?;
        match response {
            Response::BandMode { mode } => Ok(mode),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the single-band or dual-band selection (DL write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_band_mode(&mut self, mode: BandMode) -> Result<(), Error> {
        tracing::debug!(?mode, "setting band presentation");
        let response = self.execute(Command::SetBandMode { mode }).await?;
        match response {
            Response::BandMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "BandMode".into(),
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

    /// Read the auto-info mode (bare AI read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_auto_info(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading auto-info mode");
        let response = self.execute(Command::GetAutoInfo).await?;
        match response {
            Response::AutoInfo { enabled } => {
                self.auto_info_enabled = enabled;
                Ok(enabled)
            }
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AutoInfo".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the auto-info mode (AI write).
    ///
    /// When enabled (`AI 1`), firmware notification wrappers can push AG, BC,
    /// BY, FS, FT, IO, MD, SF, SM, VM, DL, FR, and FQ updates for the current
    /// serial interface. The trigger conditions for each wrapper still require
    /// hardware qualification. SQ is not in that statically proven wrapper
    /// set and has no committed raw push capture.
    ///
    /// `execute` routes unsolicited frames it encounters to
    /// the broadcast channel returned by [`subscribe`](Self::subscribe).
    /// `Radio` does not yet own an idle background reader, so that subscription
    /// is not an always-on event stream while no command is executing.
    ///
    /// # Wire format
    ///
    /// `AI 0\r` (disable) or `AI 1\r` (enable).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_auto_info(&mut self, enabled: bool) -> Result<(), Error> {
        tracing::info!(enabled, "setting auto-info mode");
        let response = self.execute(Command::SetAutoInfo { enabled }).await?;
        match response {
            Response::AutoInfo {
                enabled: response_enabled,
            } if response_enabled == enabled => {}
            Response::AutoInfoAck => {
                // A bare `AI` echo proves only that the command was accepted.
                // Read the state before caching or reporting success.
                let observed = self.get_auto_info().await?;
                if observed != enabled {
                    return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                        expected: format!("AutoInfo {{ enabled: {enabled} }}"),
                        actual: format!("AutoInfo {{ enabled: {observed} }}").into_bytes(),
                    }));
                }
            }
            other => {
                return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                    expected: format!("AutoInfo {{ enabled: {enabled} }}"),
                    actual: format!("{other:?}").into_bytes(),
                }));
            }
        }
        // Remembered only after an exact echo or successful readback so
        // `Radio::reconnect` never re-asserts an unproven state.
        self.auto_info_enabled = enabled;
        Ok(())
    }

    /// Get the radio's typed market region and hardware variant (TY read).
    ///
    /// For example, `TY K,2` becomes a [`RadioType`] containing
    /// [`RadioRegion::UnitedStates`](crate::types::RadioRegion::UnitedStates)
    /// and hardware variant `2`. The variant remains opaque because retained
    /// evidence establishes its one-nibble wire domain but not semantic names
    /// for all sixteen values.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_radio_type(&mut self) -> Result<RadioType, Error> {
        tracing::debug!("reading radio type/region");
        let response = self.execute(Command::GetRadioType).await?;
        match response {
            Response::RadioType(radio_type) => Ok(radio_type),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "RadioType".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the USB Out select state (IO read).
    ///
    /// Menu 102 and the Operating Tips describe these selections:
    /// 0 = AF (audio frequency output), 1 = IF (12 kHz centered IF signal
    /// for SSB/CW/AM, 15 kHz bandwidth), 2 = Detect (pre-detection signal).
    ///
    /// Menu 102 (USB Out Select) controls this. IF/Detect output is only
    /// available when in Single Band mode on Band B.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_usb_audio_output(&mut self) -> Result<UsbAudioOutput, Error> {
        tracing::debug!("reading USB audio output selection");
        let response = self.execute(Command::GetUsbAudioOutput).await?;
        match response {
            Response::UsbAudioOutput { output } => Ok(output),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "UsbAudioOutput".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the USB Out select state (IO write).
    ///
    /// See [`get_usb_audio_output`](Self::get_usb_audio_output) for value meanings.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_usb_audio_output(&mut self, output: UsbAudioOutput) -> Result<(), Error> {
        tracing::debug!(?output, "setting USB audio output selection");
        let response = self.execute(Command::SetUsbAudioOutput { output }).await?;
        match response {
            Response::UsbAudioOutput { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "UsbAudioOutput".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Query SD-card presence (SD read).
    ///
    /// MCP programming mode uses the distinct private `0M PROGRAM` command.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_sd_status(&mut self) -> Result<bool, Error> {
        tracing::debug!("reading SD-card presence");
        let response = self.execute(Command::GetSdCard).await?;
        match response {
            Response::SdCard { present } => Ok(present),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "SdCard".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    // -----------------------------------------------------------------------
    // MCP-based setting writes (for settings where CAT writes are rejected)
    //
    // Several MCP settings have no matching CAT setting command. Historically
    // guessed mnemonics collide with unrelated operations: bare BE transmits
    // an APRS beacon, BL reads battery level, and DW steps frequency down.
    // These methods bypass CAT entirely and write verified MCP offsets.
    //
    // Each method enters MCP programming mode, reads the containing page,
    // modifies the target byte, writes the page back, exits, waits for USB
    // re-enumeration, and restores a qualified CAT session before returning.
    // -----------------------------------------------------------------------

    /// Set key beep on/off via MCP memory write.
    ///
    /// CAT `BE` is an APRS beacon transmit action; it does not control key
    /// beeps. This method writes directly to the verified MCP offset
    /// (`0x1071`) instead.
    ///
    /// # Connection lifetime
    ///
    /// This enters and exits MCP programming mode. The exit path waits for
    /// USB re-enumeration and restores a qualified CAT session before this
    /// method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading or writing the
    /// setting page, verifying the write, exiting, or reconnecting fails.
    pub async fn set_beep_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        use super::mcp_offsets;

        tracing::info!(
            enabled,
            offset = mcp_offsets::BEEP,
            "setting key beep via MCP"
        );
        self.modify_memory_page(
            programming::WritableMcpPage::new(mcp_offsets::page(mcp_offsets::BEEP))?,
            |data| {
                data[const { mcp_offsets::byte_index(mcp_offsets::BEEP) }] = u8::from(enabled);
            },
        )
        .await
    }

    /// Set beep volume level via MCP memory write.
    ///
    /// No CAT command controls beep volume; CAT `BE` transmits an APRS
    /// beacon. This method writes directly to verified MCP offset (`0x1072`).
    /// [`LinkedVolumeLevel::VOLUME_LINK`] follows the radio's main volume;
    /// fixed levels 1–7 are constructed with [`LinkedVolumeLevel::fixed`].
    ///
    /// # Connection lifetime
    ///
    /// This enters and exits MCP programming mode. The exit path waits for
    /// USB re-enumeration and restores a qualified CAT session before this
    /// method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading or writing the
    /// setting page, verifying the write, exiting, or reconnecting fails.
    pub async fn set_beep_volume_via_mcp(
        &mut self,
        volume: LinkedVolumeLevel,
    ) -> Result<(), Error> {
        use super::mcp_offsets;

        let raw_volume = u8::from(volume);

        tracing::info!(
            volume = raw_volume,
            offset = mcp_offsets::BEEP_VOLUME,
            "setting beep volume via MCP"
        );
        self.modify_memory_page(
            programming::WritableMcpPage::new(mcp_offsets::page(mcp_offsets::BEEP_VOLUME))?,
            |data| {
                data[const { mcp_offsets::byte_index(mcp_offsets::BEEP_VOLUME) }] = raw_volume;
            },
        )
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
    /// This enters and exits MCP programming mode. The exit path waits for
    /// USB re-enumeration and restores a qualified CAT session before this
    /// method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading or writing the
    /// setting page, verifying the write, exiting, or reconnecting fails.
    pub async fn set_vox_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        use super::mcp_offsets;

        tracing::info!(
            enabled,
            offset = mcp_offsets::VOX,
            "setting VOX enable via MCP"
        );
        self.modify_memory_page(
            programming::WritableMcpPage::new(mcp_offsets::page(mcp_offsets::VOX))?,
            |data| {
                data[const { mcp_offsets::byte_index(mcp_offsets::VOX) }] = u8::from(enabled);
            },
        )
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
    /// This enters and exits MCP programming mode. The exit path waits for
    /// USB re-enumeration and restores a qualified CAT session before this
    /// method returns.
    ///
    /// # Errors
    ///
    /// Returns an error if entering programming mode, reading or writing the
    /// setting page, verifying the write, exiting, or reconnecting fails.
    pub async fn set_bluetooth_via_mcp(&mut self, enabled: bool) -> Result<(), Error> {
        use super::mcp_offsets;

        tracing::info!(
            enabled,
            offset = mcp_offsets::BLUETOOTH,
            "setting Bluetooth via MCP"
        );
        self.modify_memory_page(
            programming::WritableMcpPage::new(mcp_offsets::page(mcp_offsets::BLUETOOTH))?,
            |data| {
                data[const { mcp_offsets::byte_index(mcp_offsets::BLUETOOTH) }] = u8::from(enabled);
            },
        )
        .await
    }
}
