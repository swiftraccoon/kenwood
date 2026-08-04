//! Scan-related radio methods plus MW/SW antenna selection.
//!
//! # Single Band Display (per Operating Tips §5.10.4)
//!
//! Menu No. 904 controls the Single Band Display information line:
//! Off, GPS (Altitude), GPS (Ground Speed), Date, or Demodulation Mode.

use crate::error::{Error, ProtocolError};
use crate::protocol::programming;
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{AntennaInput, Band, ScanResumeMethod, StepSize};

use super::Radio;

impl<T: Transport> Radio<T> {
    /// Set the analog scan-resume method through Menu 130's MCP cell.
    ///
    /// This changes `radio.ScanResumeAnalog` at exact MCP offset `0x100C`.
    /// This method deliberately writes the exact Menu 130 cell instead of the
    /// `SR` CAT operation so the independent analog and digital settings remain
    /// explicit in the API. Firmware analysis identifies `SR 0/1/2` as the
    /// Time/Carrier/Seek scan-resume operation, not a reset.
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
    pub async fn set_analog_scan_resume_via_mcp(
        &mut self,
        method: ScanResumeMethod,
    ) -> Result<(), Error> {
        const PAGE: u16 = 0x0010;
        const BYTE_INDEX: usize = 0x0C;

        tracing::info!(
            ?method,
            offset = 0x100C,
            "setting analog scan resume via MCP"
        );
        self.modify_memory_page(programming::WritableMcpPage::new(PAGE)?, |data| {
            data[BYTE_INDEX] = method.as_raw();
        })
        .await
    }

    /// Set the digital scan-resume method through Menu 131's MCP cell.
    ///
    /// This changes `radio.ScanResumeDigital` at exact MCP offset `0x100D`.
    /// It is intentionally separate from
    /// [`set_analog_scan_resume_via_mcp`](Self::set_analog_scan_resume_via_mcp)
    /// because the radio stores independent values for analog and DV/DR
    /// scanning.
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
    pub async fn set_digital_scan_resume_via_mcp(
        &mut self,
        method: ScanResumeMethod,
    ) -> Result<(), Error> {
        const PAGE: u16 = 0x0010;
        const BYTE_INDEX: usize = 0x0D;

        tracing::info!(
            ?method,
            offset = 0x100D,
            "setting digital scan resume via MCP"
        );
        self.modify_memory_page(programming::WritableMcpPage::new(PAGE)?, |data| {
            data[BYTE_INDEX] = method.as_raw();
        })
        .await
    }

    /// Get the step size for a band (SF read).
    ///
    /// Firmware-verified: SF = Step Size. `SF band\r` returns `SF band,step`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_step_size(&mut self, band: Band) -> Result<StepSize, Error> {
        tracing::debug!(?band, "reading step size");
        let response = self.execute(Command::GetStepSize { band }).await?;
        match response {
            Response::StepSize {
                band: response_band,
                step,
            } if response_band == band => Ok(step),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("StepSize {{ band: {band:?} }}"),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the step size for a band (SF write).
    ///
    /// Firmware-verified: SF = Step Size. `SF band,step\r` (band 0-1, step 0-11).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_step_size(&mut self, band: Band, step: StepSize) -> Result<(), Error> {
        tracing::info!(?band, ?step, "setting step size");
        let response = self.execute(Command::SetStepSize { band, step }).await?;
        match response {
            Response::StepSize { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "StepSize".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the MW/SW receive antenna selection (BS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_antenna_input(&mut self) -> Result<AntennaInput, Error> {
        tracing::debug!("reading MW/SW antenna selection");
        let response = self.execute(Command::GetAntennaInput).await?;
        match response {
            Response::AntennaInput { input } => Ok(input),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AntennaInput".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Select the MW/SW receive antenna (BS write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_antenna_input(&mut self, input: AntennaInput) -> Result<(), Error> {
        tracing::info!(?input, "setting MW/SW antenna selection");
        let response = self.execute(Command::SetAntennaInput { input }).await?;
        match response {
            Response::AntennaInput { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "AntennaInput".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }
}
