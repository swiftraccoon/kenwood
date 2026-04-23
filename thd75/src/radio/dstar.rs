//! D-STAR (Digital Smart Technologies for Amateur Radio) subsystem methods.
//!
//! D-STAR is a digital voice and data protocol developed by JARL (Japan Amateur Radio League).
//! The TH-D75 supports D-STAR voice (DV mode) and data, including gateway linking for
//! internet-connected repeater access.
//!
//! # Command relationships
//!
//! - **DS**: selects the active D-STAR callsign slot (which stored callsign configuration to use)
//! - **CS**: selects the active callsign slot number (0-10) — similar to DS but for the CS
//!   slot register. The actual callsign text is read via DC.
//! - **DC**: reads D-STAR callsign data for a given slot (1-6). This command lives in
//!   [`audio.rs`](super) because it was discovered during audio subsystem probing — the DC
//!   mnemonic is overloaded on the D75 compared to the D74.
//! - **GW**: D-STAR gateway setting for repeater linking

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{CallsignSlot, DstarSlot, DvGatewayMode};

use super::Radio;

/// D-STAR callsign slot 1 (URCALL / destination).
const SLOT_URCALL: DstarSlot = match DstarSlot::new(1) {
    Ok(s) => s,
    Err(_) => unreachable!(),
};

/// D-STAR callsign slot 2 (RPT1 / access repeater).
const SLOT_RPT1: DstarSlot = match DstarSlot::new(2) {
    Ok(s) => s,
    Err(_) => unreachable!(),
};

/// D-STAR callsign slot 3 (RPT2 / gateway repeater).
const SLOT_RPT2: DstarSlot = match DstarSlot::new(3) {
    Ok(s) => s,
    Err(_) => unreachable!(),
};

impl<T: Transport> Radio<T> {
    /// Get the active D-STAR callsign slot (DS read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_dstar_slot(&mut self) -> Result<DstarSlot, Error> {
        tracing::debug!("reading D-STAR callsign slot");
        let response = self.execute(Command::GetDstarSlot).await?;
        match response {
            Response::DstarSlot { slot } => Ok(slot),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DstarSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the active callsign slot number (CS bare read).
    ///
    /// CS returns a slot number (0-10), NOT the callsign text itself.
    /// The actual callsign text is accessible via the CS callsign slots.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_active_callsign_slot(&mut self) -> Result<CallsignSlot, Error> {
        tracing::debug!("reading active callsign slot");
        let response = self.execute(Command::GetActiveCallsignSlot).await?;
        match response {
            Response::ActiveCallsignSlot { slot } => Ok(slot),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ActiveCallsignSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the active callsign slot (CS write).
    ///
    /// Selects which callsign slot is active. The callsign text itself
    /// is read via DC (D-STAR callsign) slots 1-6.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_active_callsign_slot(&mut self, slot: CallsignSlot) -> Result<(), Error> {
        tracing::info!(?slot, "setting active callsign slot");
        let response = self
            .execute(Command::SetActiveCallsignSlot { slot })
            .await?;
        match response {
            Response::ActiveCallsignSlot { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "ActiveCallsignSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the active D-STAR callsign slot (DS write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_dstar_slot(&mut self, slot: DstarSlot) -> Result<(), Error> {
        tracing::info!(?slot, "setting D-STAR callsign slot");
        let response = self.execute(Command::SetDstarSlot { slot }).await?;
        match response {
            Response::DstarSlot { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "DstarSlot".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Get the gateway value (GW read).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_gateway(&mut self) -> Result<DvGatewayMode, Error> {
        tracing::debug!("reading D-STAR gateway");
        let response = self.execute(Command::GetGateway).await?;
        match response {
            Response::Gateway { value } => Ok(value),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Gateway".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set the gateway value (GW write).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn set_gateway(&mut self, value: DvGatewayMode) -> Result<(), Error> {
        tracing::info!(?value, "setting D-STAR gateway mode");
        let response = self.execute(Command::SetGateway { value }).await?;
        match response {
            Response::Gateway { .. } => Ok(()),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "Gateway".into(),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    // -----------------------------------------------------------------------
    // High-level callsign read/write helpers
    // -----------------------------------------------------------------------

    /// Read the current URCALL (destination) callsign from slot 1.
    ///
    /// In D-STAR, the URCALL field determines the routing behaviour of your
    /// transmission (per User Manual Chapter 16):
    ///
    /// - `"CQCQCQ  "` — general CQ call (local or via gateway)
    /// - A specific callsign — callsign routing through the D-STAR network
    /// - A reflector command — link/unlink/info/echo operations
    ///
    /// Returns `(callsign, suffix)` where both are as stored on the radio
    /// (8-char callsign, up to 4-char suffix).
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_urcall(&mut self) -> Result<(String, String), Error> {
        self.get_dstar_callsign(SLOT_URCALL).await
    }

    /// Set the URCALL (destination) callsign in slot 1.
    ///
    /// The URCALL field controls D-STAR routing behaviour. Common values:
    ///
    /// - CQ call: `set_urcall("CQCQCQ", "")` — general call
    /// - Callsign routing: `set_urcall("KQ4NIT", "")` — route to a station
    /// - Reflector link: `set_urcall("REF030", "CL")` — connect module C, link
    /// - Reflector unlink: `set_urcall("       U", "")` — 7 spaces + U
    ///
    /// The callsign is space-padded to 8 characters and the suffix to 4
    /// characters before writing to the radio.
    ///
    /// # Errors
    ///
    /// Returns an error if the callsign exceeds 8 characters, the suffix
    /// exceeds 4 characters, or the command fails.
    pub async fn set_urcall(&mut self, callsign: &str, suffix: &str) -> Result<(), Error> {
        let padded_cs = pad_callsign(callsign)?;
        let padded_sfx = pad_suffix(suffix)?;
        self.set_dstar_callsign(SLOT_URCALL, &padded_cs, &padded_sfx)
            .await
    }

    /// Read the RPT1 (access repeater) callsign from slot 2.
    ///
    /// RPT1 is the local repeater that your radio transmits to. In the
    /// D-STAR routing model, RPT1 receives your signal over RF and either
    /// plays it locally or forwards it to RPT2 for gateway routing.
    ///
    /// Returns `(callsign, suffix)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_rpt1(&mut self) -> Result<(String, String), Error> {
        self.get_dstar_callsign(SLOT_RPT1).await
    }

    /// Set the RPT1 (access repeater) callsign in slot 2.
    ///
    /// RPT1 should be set to the callsign of your local D-STAR repeater's
    /// RF module (e.g. `"W4BFB  C"` for a 2m module). The module letter
    /// is part of the 8-character callsign field, not the suffix.
    ///
    /// The callsign is space-padded to 8 characters and the suffix to 4
    /// characters before writing.
    ///
    /// # Errors
    ///
    /// Returns an error if the callsign exceeds 8 characters, the suffix
    /// exceeds 4 characters, or the command fails.
    pub async fn set_rpt1(&mut self, callsign: &str, suffix: &str) -> Result<(), Error> {
        let padded_cs = pad_callsign(callsign)?;
        let padded_sfx = pad_suffix(suffix)?;
        self.set_dstar_callsign(SLOT_RPT1, &padded_cs, &padded_sfx)
            .await
    }

    /// Read the RPT2 (gateway repeater) callsign from slot 3.
    ///
    /// RPT2 is the gateway repeater that forwards your signal to the D-STAR
    /// network. For gateway-linked calls, RPT2 is typically the repeater's
    /// gateway callsign (module G). For local-only calls, RPT2 can be left
    /// blank or set to the same repeater.
    ///
    /// Returns `(callsign, suffix)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_rpt2(&mut self) -> Result<(String, String), Error> {
        self.get_dstar_callsign(SLOT_RPT2).await
    }

    /// Set the RPT2 (gateway repeater) callsign in slot 3.
    ///
    /// For gateway-linked operation, set RPT2 to the repeater's gateway
    /// module (e.g. `"W4BFB  G"`). For local-only simplex or repeater use,
    /// RPT2 can be blank.
    ///
    /// The callsign is space-padded to 8 characters and the suffix to 4
    /// characters before writing.
    ///
    /// # Errors
    ///
    /// Returns an error if the callsign exceeds 8 characters, the suffix
    /// exceeds 4 characters, or the command fails.
    pub async fn set_rpt2(&mut self, callsign: &str, suffix: &str) -> Result<(), Error> {
        let padded_cs = pad_callsign(callsign)?;
        let padded_sfx = pad_suffix(suffix)?;
        self.set_dstar_callsign(SLOT_RPT2, &padded_cs, &padded_sfx)
            .await
    }

    // -----------------------------------------------------------------------
    // Reflector control helpers
    // -----------------------------------------------------------------------

    /// Connect to a D-STAR reflector.
    ///
    /// Sets the URCALL field to the reflector callsign with a link suffix,
    /// which instructs the gateway to link to the specified reflector module.
    /// The operator must then key up (transmit briefly) to trigger the link
    /// command.
    ///
    /// # Parameters
    ///
    /// - `reflector`: Reflector callsign, e.g. `"REF030"`, `"XLX390"`, `"DCS006"`.
    ///   Padded to 8 characters.
    /// - `module`: The reflector module letter, e.g. `'C'` for module C.
    ///
    /// # Wire encoding
    ///
    /// URCALL is set to the reflector callsign (8 chars) and the suffix is
    /// set to `"{module}L  "` (module letter + 'L' for link, space-padded
    /// to 4 chars). For example, `connect_reflector("REF030", 'C')` sets
    /// URCALL to `"REF030  "` with suffix `"CL  "`.
    ///
    /// # Errors
    ///
    /// Returns an error if the reflector callsign exceeds 8 characters,
    /// or the command fails.
    pub async fn connect_reflector(&mut self, reflector: &str, module: char) -> Result<(), Error> {
        let suffix = format!("{module}L");
        self.set_urcall(reflector, &suffix).await
    }

    /// Disconnect from the current D-STAR reflector.
    ///
    /// Sets URCALL to the unlink command (`"       U"` — 7 spaces followed
    /// by 'U') with a blank suffix. The operator must then key up to
    /// trigger the unlink.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails.
    pub async fn disconnect_reflector(&mut self) -> Result<(), Error> {
        self.set_urcall("       U", "").await
    }

    /// Set URCALL to CQCQCQ for a general CQ call.
    ///
    /// This configures the radio for a local or gateway CQ call. Whether
    /// the call goes through the gateway depends on the RPT2 setting:
    /// if RPT2 is set to the gateway module (G), the call is relayed to
    /// all linked stations/reflectors.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails.
    pub async fn set_cq(&mut self) -> Result<(), Error> {
        self.set_urcall("CQCQCQ", "").await
    }

    /// Set URCALL for callsign routing (individual call).
    ///
    /// Routes your transmission to a specific station through the D-STAR
    /// network. The gateway will look up the destination callsign in the
    /// D-STAR registration database and forward your audio to the last
    /// repeater the target station was heard on.
    ///
    /// RPT2 must be set to the gateway module for routing to work.
    ///
    /// # Errors
    ///
    /// Returns an error if the callsign exceeds 8 characters or the
    /// command fails.
    pub async fn route_to_callsign(&mut self, callsign: &str) -> Result<(), Error> {
        self.set_urcall(callsign, "").await
    }

    // -----------------------------------------------------------------------
    // D-STAR text messaging
    // -----------------------------------------------------------------------
    //
    // The TH-D75 does not expose a CAT command for sending D-STAR slow-data
    // text messages. The `MS` command is APRS-only (position source / message
    // send). D-STAR slow-data messages are embedded in the DV voice stream
    // and are not accessible through the serial CAT protocol.
    //
    // To send D-STAR text, use the radio's front-panel menu or a D-STAR
    // application (BlueDV, etc.) over Bluetooth/USB data mode.
}

/// Pad a callsign to exactly 8 characters with trailing spaces.
///
/// # Errors
///
/// Returns [`ProtocolError::FieldParse`] if the callsign exceeds 8 characters.
fn pad_callsign(callsign: &str) -> Result<String, Error> {
    if callsign.len() > 8 {
        return Err(Error::Protocol(ProtocolError::FieldParse {
            command: "DC".into(),
            field: "callsign".into(),
            detail: format!("callsign {:?} is {} chars, max 8", callsign, callsign.len()),
        }));
    }
    Ok(format!("{callsign:<8}"))
}

/// Pad a suffix to exactly 4 characters with trailing spaces.
///
/// # Errors
///
/// Returns [`ProtocolError::FieldParse`] if the suffix exceeds 4 characters.
fn pad_suffix(suffix: &str) -> Result<String, Error> {
    if suffix.len() > 4 {
        return Err(Error::Protocol(ProtocolError::FieldParse {
            command: "DC".into(),
            field: "suffix".into(),
            detail: format!("suffix {:?} is {} chars, max 4", suffix, suffix.len()),
        }));
    }
    Ok(format!("{suffix:<4}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // DC wire format: "DC slot,callsign,suffix\r"
    // Response echoes the same format back.

    #[tokio::test]
    async fn get_urcall_reads_slot_1() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1\r", b"DC 1,CQCQCQ  ,    \r");

        let mut radio = Radio::connect(mock).await?;
        let (callsign, suffix) = radio.get_urcall().await?;
        assert_eq!(callsign, "CQCQCQ  ");
        assert_eq!(suffix, "    ");
        Ok(())
    }

    #[tokio::test]
    async fn set_urcall_pads_and_writes_slot_1() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1,KQ4NIT  ,    \r", b"DC 1,KQ4NIT  ,    \r");

        let mut radio = Radio::connect(mock).await?;
        radio.set_urcall("KQ4NIT", "").await?;
        Ok(())
    }

    #[tokio::test]
    async fn get_rpt1_reads_slot_2() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 2\r", b"DC 2,W4BFB  C,    \r");

        let mut radio = Radio::connect(mock).await?;
        let (callsign, _suffix) = radio.get_rpt1().await?;
        assert_eq!(callsign, "W4BFB  C");
        Ok(())
    }

    #[tokio::test]
    async fn set_rpt1_pads_and_writes_slot_2() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 2,W4BFB  C,    \r", b"DC 2,W4BFB  C,    \r");

        let mut radio = Radio::connect(mock).await?;
        radio.set_rpt1("W4BFB  C", "").await?;
        Ok(())
    }

    #[tokio::test]
    async fn get_rpt2_reads_slot_3() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 3\r", b"DC 3,W4BFB  G,    \r");

        let mut radio = Radio::connect(mock).await?;
        let (callsign, _suffix) = radio.get_rpt2().await?;
        assert_eq!(callsign, "W4BFB  G");
        Ok(())
    }

    #[tokio::test]
    async fn set_rpt2_pads_and_writes_slot_3() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 3,W4BFB  G,    \r", b"DC 3,W4BFB  G,    \r");

        let mut radio = Radio::connect(mock).await?;
        radio.set_rpt2("W4BFB  G", "").await?;
        Ok(())
    }

    #[tokio::test]
    async fn connect_reflector_sets_urcall_with_link_suffix() -> TestResult {
        let mut mock = MockTransport::new();
        // "REF030" padded to 8 = "REF030  ", suffix "CL" padded to 4 = "CL  "
        mock.expect(b"DC 1,REF030  ,CL  \r", b"DC 1,REF030  ,CL  \r");

        let mut radio = Radio::connect(mock).await?;
        radio.connect_reflector("REF030", 'C').await?;
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_reflector_sets_unlink_urcall() -> TestResult {
        let mut mock = MockTransport::new();
        // "       U" is already 8 chars, suffix "" padded to "    "
        mock.expect(b"DC 1,       U,    \r", b"DC 1,       U,    \r");

        let mut radio = Radio::connect(mock).await?;
        radio.disconnect_reflector().await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_cq_sets_cqcqcq() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1,CQCQCQ  ,    \r", b"DC 1,CQCQCQ  ,    \r");

        let mut radio = Radio::connect(mock).await?;
        radio.set_cq().await?;
        Ok(())
    }

    #[tokio::test]
    async fn route_to_callsign_sets_urcall() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1,KQ4NIT  ,    \r", b"DC 1,KQ4NIT  ,    \r");

        let mut radio = Radio::connect(mock).await?;
        radio.route_to_callsign("KQ4NIT").await?;
        Ok(())
    }

    #[test]
    fn pad_callsign_valid() -> TestResult {
        assert_eq!(pad_callsign("CQCQCQ")?, "CQCQCQ  ");
        assert_eq!(pad_callsign("KQ4NIT")?, "KQ4NIT  ");
        assert_eq!(pad_callsign("       U")?, "       U");
        assert_eq!(pad_callsign("")?, "        ");
        Ok(())
    }

    #[test]
    fn pad_callsign_too_long() {
        assert!(pad_callsign("123456789").is_err());
    }

    #[test]
    fn pad_suffix_valid() -> TestResult {
        assert_eq!(pad_suffix("")?, "    ");
        assert_eq!(pad_suffix("CL")?, "CL  ");
        assert_eq!(pad_suffix("D75A")?, "D75A");
        Ok(())
    }

    #[test]
    fn pad_suffix_too_long() {
        assert!(pad_suffix("12345").is_err());
    }
}
