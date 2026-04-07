//! Factory service mode commands for calibration, testing, and diagnostics.
//!
//! # Safety
//!
//! These commands are intended for factory use only. Incorrect use can:
//! - **Corrupt factory calibration** (0R, 1A-1W) — may require professional recalibration
//! - **Brick the radio** (1F) — raw flash write can overwrite boot code
//! - **Change the serial number** (1I) — may void warranty
//!
//! Service mode is entered by sending `0G KENWOOD` and exited with bare `0G`.
//! While in service mode, the standard CAT command table is replaced with the
//! service mode table — normal commands will not work until service mode is exited.
//!
//! All 20 service mode commands were discovered via Ghidra decompilation of the
//! TH-D75 V1.03 firmware. The service mode table is at address 0xC006F288 with
//! 34 entries (20 service + 14 remapped standard commands).

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Enter factory service mode (0G KENWOOD).
    ///
    /// Switches the radio from the standard 53-command CAT table to the
    /// 34-entry service mode table. Normal CAT commands will not work until
    /// service mode is exited with [`exit_service_mode`](Self::exit_service_mode).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn enter_service_mode(&mut self) -> Result<(), Error> {
        tracing::info!("entering factory service mode (0G KENWOOD)");
        let response = self.execute(Command::EnterServiceMode).await?;
        match response {
            Response::ServiceMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Exit factory service mode (0G bare).
    ///
    /// Restores the standard CAT command table. Normal CAT commands will
    /// work again after this call.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn exit_service_mode(&mut self) -> Result<(), Error> {
        tracing::info!("exiting factory service mode (bare 0G)");
        let response = self.execute(Command::ExitServiceMode).await?;
        match response {
            Response::ServiceMode { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceMode".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read factory calibration data (0S).
    ///
    /// Returns 200 bytes of hex-encoded factory calibration data (118 bytes
    /// from flash 0x4E000 + 82 bytes from a second address).
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn read_calibration_data(&mut self) -> Result<String, Error> {
        tracing::debug!("reading factory calibration data (0S)");
        let response = self.execute(Command::ReadCalibrationData).await?;
        match response {
            Response::ServiceCalibrationData { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationData".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write factory calibration data (0R).
    ///
    /// Writes 200 bytes of calibration data. The `data` parameter must be
    /// exactly 400 hex characters encoding 200 bytes.
    ///
    /// # Safety
    ///
    /// **CRITICAL: Can corrupt factory calibration.** Always read calibration
    /// first with [`read_calibration_data`](Self::read_calibration_data) and
    /// keep a backup before writing.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn write_calibration_data(&mut self, data: &str) -> Result<(), Error> {
        tracing::warn!("writing factory calibration data (0R) — CRITICAL OPERATION");
        let response = self
            .execute(Command::WriteCalibrationData {
                data: data.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceCalibrationWrite { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationWrite".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get service/MCP status (0E in service mode).
    ///
    /// Reads 3 bytes from hardware status register at address 0x110.
    /// In normal mode, 0E returns `N` (not available). In service mode,
    /// it returns actual status data.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_service_status(&mut self) -> Result<String, Error> {
        tracing::debug!("reading service status (0E)");
        let response = self.execute(Command::GetServiceStatus).await?;
        match response {
            // In service mode, the user parser handles 0E and returns McpStatus.
            Response::McpStatus { value } => Ok(value),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "McpStatus".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read/write calibration parameter 1A.
    ///
    /// Delegates to the firmware's command executor for calibration access.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_1a(&mut self) -> Result<String, Error> {
        tracing::debug!("service calibration 1A");
        let response = self.execute(Command::ServiceCalibrate1A).await?;
        match response {
            Response::ServiceCalibrationParam { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read/write calibration parameter 1D.
    ///
    /// Same executor-based pattern as 1A.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_1d(&mut self) -> Result<String, Error> {
        tracing::debug!("service calibration 1D");
        let response = self.execute(Command::ServiceCalibrate1D).await?;
        match response {
            Response::ServiceCalibrationParam { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read calibration parameter 1E, or write with a 3-character value.
    ///
    /// When `value` is `None`, sends a bare read (`1E\r`).
    /// When `value` is `Some`, sends `1E XXX\r` (write form).
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_1e(&mut self, value: Option<&str>) -> Result<String, Error> {
        tracing::debug!(?value, "service calibration 1E");
        let response = self
            .execute(Command::ServiceCalibrate1E {
                value: value.map(str::to_owned),
            })
            .await?;
        match response {
            Response::ServiceCalibrationParam { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read/write calibration parameter 1N.
    ///
    /// Same executor-based pattern as 1A.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_1n(&mut self) -> Result<String, Error> {
        tracing::debug!("service calibration 1N");
        let response = self.execute(Command::ServiceCalibrate1N).await?;
        match response {
            Response::ServiceCalibrationParam { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read calibration parameter 1V, or write with a 3-character value.
    ///
    /// Same dual-mode pattern as 1E.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_1v(&mut self, value: Option<&str>) -> Result<String, Error> {
        tracing::debug!(?value, "service calibration 1V");
        let response = self
            .execute(Command::ServiceCalibrate1V {
                value: value.map(str::to_owned),
            })
            .await?;
        match response {
            Response::ServiceCalibrationParam { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write calibration parameter 1W (write only).
    ///
    /// Single-character parameter, likely a mode or flag toggle.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_1w(&mut self, value: &str) -> Result<String, Error> {
        tracing::debug!(value, "service calibration 1W");
        let response = self
            .execute(Command::ServiceCalibrate1W {
                value: value.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceCalibrationParam { data, .. } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write factory callsign/serial number (1I).
    ///
    /// The `id` parameter must be exactly 8 alphanumeric characters and
    /// `code` must be exactly 3 alphanumeric characters.
    ///
    /// # Safety
    ///
    /// **HIGH RISK: Changes the radio's factory serial number / callsign.**
    /// This may void the warranty and could cause regulatory issues.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_write_id(&mut self, id: &str, code: &str) -> Result<(), Error> {
        tracing::warn!(id, code, "writing factory ID (1I) — HIGH RISK OPERATION");
        let response = self
            .execute(Command::ServiceWriteId {
                id: id.to_owned(),
                code: code.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceWriteId { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceWriteId".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read flash memory (1F bare read).
    ///
    /// The read behavior depends on the firmware executor's internal state.
    ///
    /// # Safety
    ///
    /// While reading is generally safe, this accesses raw flash memory.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_flash_read(&mut self) -> Result<String, Error> {
        tracing::debug!("reading flash memory (1F)");
        let response = self.execute(Command::ServiceFlashRead).await?;
        match response {
            Response::ServiceFlash { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceFlash".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write raw flash memory (1F write).
    ///
    /// The `address` must be a 6-digit hex string (max 0x04FFFF) and `data`
    /// must be hex-encoded bytes. Address + data length must not exceed 0x50000.
    ///
    /// # Safety
    ///
    /// **CRITICAL: Can brick the radio.** Raw flash writes can overwrite
    /// boot code, calibration data, or firmware. There is no recovery
    /// mechanism short of JTAG or factory repair.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_flash_write(&mut self, address: &str, data: &str) -> Result<(), Error> {
        tracing::warn!(
            address,
            data_len = data.len(),
            "writing raw flash memory (1F) — CRITICAL OPERATION"
        );
        let response = self
            .execute(Command::ServiceFlashWrite {
                address: address.to_owned(),
                data: data.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceFlash { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceFlash".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Generic write via executor (0W).
    ///
    /// The exact operation depends on the firmware executor's internal state.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_write_config(&mut self) -> Result<String, Error> {
        tracing::debug!("service write config (0W)");
        let response = self.execute(Command::ServiceWriteConfig).await?;
        match response {
            Response::ServiceWriteConfig { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceWriteConfig".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Select service mode band (0Y).
    ///
    /// Band 0 and band 1 activate different receiver chain code paths
    /// in the firmware.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_band_select(&mut self, band: u8) -> Result<(), Error> {
        tracing::debug!(band, "service band select (0Y)");
        let response = self.execute(Command::ServiceBandSelect { band }).await?;
        match response {
            Response::ServiceBandSelect { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceBandSelect".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read bulk EEPROM/calibration data (9E).
    ///
    /// Reads up to 256 bytes from the specified address. The `address`
    /// parameter is a 6-digit hex string and `length` is a 2-digit hex
    /// string (00 = 256 bytes). Address + length must not exceed 0x50000.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_read_eeprom(
        &mut self,
        address: &str,
        length: &str,
    ) -> Result<String, Error> {
        tracing::debug!(address, length, "reading EEPROM data (9E)");
        let response = self
            .execute(Command::ServiceReadEeprom {
                address: address.to_owned(),
                length: length.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceEepromData { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceEepromData".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read calibration data at current offset (9R).
    ///
    /// Returns 4 bytes of formatted calibration data. The offset is
    /// determined by firmware internal state.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_read_eeprom_addr(&mut self) -> Result<String, Error> {
        tracing::debug!("reading EEPROM at current offset (9R)");
        let response = self.execute(Command::ServiceReadEepromAddr).await?;
        match response {
            Response::ServiceEepromAddr { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceEepromAddr".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get internal version/variant information (2V).
    ///
    /// Returns model code (e.g., EX-5210), build date, hardware revision,
    /// and calibration date. The `param1` is a 2-digit hex parameter and
    /// `param2` is a 3-digit hex parameter.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_get_version(
        &mut self,
        param1: &str,
        param2: &str,
    ) -> Result<String, Error> {
        tracing::debug!(param1, param2, "reading service version info (2V)");
        let response = self
            .execute(Command::ServiceGetVersion {
                param1: param1.to_owned(),
                param2: param2.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceVersion { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceVersion".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get hardware register / GPIO status (1G).
    ///
    /// Returns hex-encoded hardware register values. Used for factory
    /// testing of GPIO and peripheral status.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_get_hardware(&mut self) -> Result<String, Error> {
        tracing::debug!("reading hardware register status (1G)");
        let response = self.execute(Command::ServiceGetHardware).await?;
        match response {
            Response::ServiceHardware { data } => Ok(data),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceHardware".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Write new D75-specific calibration parameter (1C).
    ///
    /// The `value` must be a 3-digit hex string (max 0xFF). This command
    /// is new in the D75 (not present in D74) and may be related to the
    /// 220 MHz band or enhanced DSP.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_calibrate_new(&mut self, value: &str) -> Result<(), Error> {
        tracing::debug!(value, "service calibration 1C (D75-specific)");
        let response = self
            .execute(Command::ServiceCalibrateNew {
                value: value.to_owned(),
            })
            .await?;
        match response {
            Response::ServiceCalibrationParam { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Read or write dynamic-length hardware configuration (1U).
    ///
    /// When `data` is `None`, sends a bare read. When `data` is `Some`,
    /// sends a write with the provided data. The expected wire length
    /// is determined dynamically by the firmware reading a hardware register.
    ///
    /// Requires service mode. Call [`enter_service_mode`](Self::enter_service_mode) first.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn service_dynamic_param(&mut self, data: Option<&str>) -> Result<String, Error> {
        tracing::debug!(?data, "service dynamic parameter (1U)");
        let response = self
            .execute(Command::ServiceDynamicParam {
                data: data.map(str::to_owned),
            })
            .await?;
        match response {
            Response::ServiceCalibrationParam { data: resp, .. } => Ok(resp),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ServiceCalibrationParam".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::radio::Radio;
    use crate::transport::MockTransport;

    #[tokio::test]
    async fn service_enter_and_exit() {
        let mut mock = MockTransport::new();
        mock.expect(b"0G KENWOOD\r", b"0G KENWOOD\r");
        mock.expect(b"0G\r", b"0G \r");
        let mut radio = Radio::connect(mock).await.unwrap();
        radio.enter_service_mode().await.unwrap();
        radio.exit_service_mode().await.unwrap();
    }

    #[tokio::test]
    async fn service_band_select() {
        let mut mock = MockTransport::new();
        mock.expect(b"0Y 0\r", b"0Y 0\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        radio.service_band_select(0).await.unwrap();
    }

    #[tokio::test]
    async fn service_band_select_band_1() {
        let mut mock = MockTransport::new();
        mock.expect(b"0Y 1\r", b"0Y 1\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        radio.service_band_select(1).await.unwrap();
    }

    #[tokio::test]
    async fn service_get_hardware() {
        let mut mock = MockTransport::new();
        mock.expect(b"1G\r", b"1G AA,BB,CC\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let data = radio.service_get_hardware().await.unwrap();
        assert_eq!(data, "AA,BB,CC");
    }

    #[tokio::test]
    async fn service_get_version() {
        let mut mock = MockTransport::new();
        mock.expect(b"2V 00,000\r", b"2V EX-5210\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let data = radio.service_get_version("00", "000").await.unwrap();
        assert_eq!(data, "EX-5210");
    }

    #[tokio::test]
    async fn service_read_calibration_data() {
        let mut mock = MockTransport::new();
        mock.expect(b"0S\r", b"0S AABBCCDD\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let data = radio.read_calibration_data().await.unwrap();
        assert_eq!(data, "AABBCCDD");
    }

    #[tokio::test]
    async fn service_read_eeprom() {
        let mut mock = MockTransport::new();
        mock.expect(b"9E 04E000,10\r", b"9E DEADBEEF\r");
        let mut radio = Radio::connect(mock).await.unwrap();
        let data = radio.service_read_eeprom("04E000", "10").await.unwrap();
        assert_eq!(data, "DEADBEEF");
    }
}
