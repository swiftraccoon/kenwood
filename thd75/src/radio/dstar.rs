//! D-STAR (Digital Smart Technologies for Amateur Radio) subsystem methods.
//!
//! D-STAR is a digital voice and data protocol developed by JARL (Japan Amateur Radio League).
//! The TH-D75 supports D-STAR voice (DV mode) and data, including gateway linking for
//! internet-connected repeater access.
//!
//! # Command relationships
//!
//! - **DS**: selects the active D-STAR callsign slot (which stored callsign settings to use)
//! - **DC**: reads or writes D-STAR callsign data for a given slot (1-6)
//! - **GW**: D-STAR gateway setting for repeater linking

use crate::error::{Error, ProtocolError};
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{
    DstarCallsign, DstarCallsignEntry, DstarSlot, DstarSuffix, DvGatewayMode, Module,
    ReflectorCallsign,
};

use super::Radio;

/// D-STAR callsign slot 1 (URCALL / destination).
const SLOT_URCALL: DstarSlot = DstarSlot::SLOT_1;

/// D-STAR callsign slot 2 (RPT1 / access repeater).
const SLOT_RPT1: DstarSlot = DstarSlot::SLOT_2;

/// D-STAR callsign slot 3 (RPT2 / gateway repeater).
const SLOT_RPT2: DstarSlot = DstarSlot::SLOT_3;

impl<T: Transport> Radio<T> {
    /// Get D-STAR callsign data for a slot (DC read).
    ///
    /// Hardware-verified: `DC slot\r` where slot is 1-6. Returns the
    /// validated callsign and suffix with wire padding removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_dstar_callsign(
        &mut self,
        slot: DstarSlot,
    ) -> Result<DstarCallsignEntry, Error> {
        tracing::debug!(?slot, "reading D-STAR callsign");
        let response = self.execute(Command::GetDstarCallsign { slot }).await?;
        match response {
            Response::DstarCallsign {
                slot: response_slot,
                callsign,
                suffix,
            } if response_slot == slot => Ok(DstarCallsignEntry::new(callsign, suffix)),
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("DstarCallsign {{ slot: {slot:?} }}"),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Set validated D-STAR callsign data for a slot (DC write).
    ///
    /// The serializer emits an exact eight-byte callsign and four-byte suffix,
    /// both right-padded with spaces.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the radio does not echo the
    /// exact requested slot, callsign, and suffix.
    pub async fn set_dstar_callsign(
        &mut self,
        slot: DstarSlot,
        callsign: DstarCallsign,
        suffix: DstarSuffix,
    ) -> Result<(), Error> {
        tracing::info!(
            ?slot,
            callsign = callsign.as_str(),
            suffix = suffix.as_str(),
            "setting D-STAR callsign"
        );
        let response = self
            .execute(Command::SetDstarCallsign {
                slot,
                callsign: callsign.clone(),
                suffix: suffix.clone(),
            })
            .await?;
        match response {
            Response::DstarCallsign {
                slot: response_slot,
                callsign: response_callsign,
                suffix: response_suffix,
            } if response_slot == slot
                && response_callsign == callsign
                && response_suffix == suffix =>
            {
                Ok(())
            }
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!(
                    "DstarCallsign {{ slot: {slot:?}, callsign: {callsign:?}, suffix: {suffix:?} }}"
                ),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

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

    /// Read the gateway value, querying firmware first when it is not cached.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CommandUnavailableOnFirmware`] without sending `GW`
    /// unless the exact cached or queried firmware identity is in
    /// [`super::STANDARD_CAT_FIRMWARE_IDENTITIES`]. On qualified firmware,
    /// returns an error if the command fails or the response is unexpected.
    pub async fn read_gateway(&mut self) -> Result<DvGatewayMode, Error> {
        self.require_firmware_command("GW", super::FirmwareProfile::supports_bare_gateway)
            .await?;
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

    // -----------------------------------------------------------------------
    // High-level callsign read/write helpers
    // -----------------------------------------------------------------------

    /// Read the current URCALL (destination) callsign from slot 1.
    ///
    /// In D-STAR, the URCALL field determines the routing behaviour of your
    /// transmission (per User Manual Chapter 16):
    ///
    /// - `"CQCQCQ  "`: general CQ call (local or via gateway)
    /// - A specific callsign: callsign routing through the D-STAR network
    /// - A reflector command: link/unlink/info/echo operations
    ///
    /// Returns the validated callsign and suffix with wire padding removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_urcall(&mut self) -> Result<DstarCallsignEntry, Error> {
        self.get_dstar_callsign(SLOT_URCALL).await
    }

    /// Set the URCALL (destination) callsign in slot 1.
    ///
    /// The URCALL field controls D-STAR routing behaviour. Common values:
    ///
    /// - CQ call: [`DstarCallsign::cqcqcq`] for a general call
    /// - Callsign routing: `DstarCallsign::new("KQ4NIT")` to route to a station
    /// - Reflector link: callsign `REF030`, suffix `CL`
    /// - Reflector unlink: callsign `"       U"`, blank suffix
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the radio does not echo the
    /// exact validated values.
    pub async fn set_urcall(
        &mut self,
        callsign: DstarCallsign,
        suffix: DstarSuffix,
    ) -> Result<(), Error> {
        self.set_dstar_callsign(SLOT_URCALL, callsign, suffix).await
    }

    /// Read the RPT1 (access repeater) callsign from slot 2.
    ///
    /// RPT1 is the local repeater that your radio transmits to. In the
    /// D-STAR routing model, RPT1 receives your signal over RF and either
    /// plays it locally or forwards it to RPT2 for gateway routing.
    ///
    /// Returns the validated callsign and suffix with wire padding removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_rpt1(&mut self) -> Result<DstarCallsignEntry, Error> {
        self.get_dstar_callsign(SLOT_RPT1).await
    }

    /// Set the RPT1 (access repeater) callsign in slot 2.
    ///
    /// RPT1 should be set to the callsign of your local D-STAR repeater's
    /// RF module (e.g. `"W4BFB  C"` for a 2m module). The module letter
    /// is part of the 8-character callsign field, not the suffix.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the radio does not echo the
    /// exact validated values.
    pub async fn set_rpt1(
        &mut self,
        callsign: DstarCallsign,
        suffix: DstarSuffix,
    ) -> Result<(), Error> {
        self.set_dstar_callsign(SLOT_RPT1, callsign, suffix).await
    }

    /// Read the RPT2 (gateway repeater) callsign from slot 3.
    ///
    /// RPT2 is the gateway repeater that forwards your signal to the D-STAR
    /// network. For gateway-linked calls, RPT2 is typically the repeater's
    /// gateway callsign (module G). For local-only calls, RPT2 can be left
    /// blank or set to the same repeater.
    ///
    /// Returns the validated callsign and suffix with wire padding removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the response is unexpected.
    pub async fn get_rpt2(&mut self) -> Result<DstarCallsignEntry, Error> {
        self.get_dstar_callsign(SLOT_RPT2).await
    }

    /// Set the RPT2 (gateway repeater) callsign in slot 3.
    ///
    /// For gateway-linked operation, set RPT2 to the repeater's gateway
    /// module (e.g. `"W4BFB  G"`). For local-only simplex or repeater use,
    /// RPT2 can be blank.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails or the radio does not echo the
    /// exact validated values.
    pub async fn set_rpt2(
        &mut self,
        callsign: DstarCallsign,
        suffix: DstarSuffix,
    ) -> Result<(), Error> {
        self.set_dstar_callsign(SLOT_RPT2, callsign, suffix).await
    }

    // -----------------------------------------------------------------------
    // Reflector control helpers
    // -----------------------------------------------------------------------

    /// Prepare the URCALL command used to link a D-STAR reflector.
    ///
    /// Sets the URCALL field to the reflector callsign with a link suffix,
    /// which instructs the gateway to link to the specified reflector module.
    /// The operator must then key up (transmit briefly) to trigger the link
    /// command.
    ///
    /// # Parameters
    ///
    /// - `reflector`: A validated reflector callsign, e.g. `REF030`, `XLX390`,
    ///   or `DCS006`. It is padded to eight characters on the wire.
    /// - `module`: A validated reflector module letter (`A` through `Z`).
    ///
    /// # Wire encoding
    ///
    /// URCALL is set to the reflector callsign (8 chars) and the suffix is
    /// set to `"{module}L  "` (module letter + 'L' for link, space-padded
    /// to 4 chars). For example, reflector `REF030` and [`Module::C`] set
    /// URCALL to `"REF030  "` with suffix `"CL  "`.
    ///
    /// # Errors
    ///
    /// Returns an error if the reflector callsign cannot be represented in a
    /// CAT callsign field, the command fails, or the radio does not echo the
    /// exact values.
    pub async fn prepare_reflector_link(
        &mut self,
        reflector: ReflectorCallsign,
        module: Module,
    ) -> Result<(), Error> {
        let callsign = DstarCallsign::try_from(reflector)?;
        self.set_urcall(callsign, DstarSuffix::reflector_link(module))
            .await
    }

    /// Prepare the URCALL command used to unlink a D-STAR reflector.
    ///
    /// Sets URCALL to the unlink command (`"       U"`, 7 spaces followed
    /// by 'U') with a blank suffix. The operator must then key up to
    /// trigger the unlink.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails.
    pub async fn prepare_reflector_unlink(&mut self) -> Result<(), Error> {
        self.set_urcall(DstarCallsign::new("       U")?, DstarSuffix::default())
            .await
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
        self.set_urcall(DstarCallsign::cqcqcq(), DstarSuffix::default())
            .await
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
    /// Returns an error if the command fails.
    pub async fn route_to_callsign(&mut self, callsign: DstarCallsign) -> Result<(), Error> {
        self.set_urcall(callsign, DstarSuffix::default()).await
    }

    // -----------------------------------------------------------------------
    // D-STAR text messaging
    // -----------------------------------------------------------------------
    //
    // The TH-D75 does not expose a CAT command for sending D-STAR slow-data
    // text messages. `MS` selects the APRS/GPS My Position entry. D-STAR
    // slow-data messages are embedded in the DV voice stream and are not
    // accessible through the serial CAT protocol.
    //
    // To send D-STAR text, use the radio's front-panel menu or a D-STAR
    // application (BlueDV, etc.) over Bluetooth/USB data mode.
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

        let mut radio = Radio::new(mock);
        let entry = radio.get_urcall().await?;
        assert_eq!(entry.callsign, DstarCallsign::cqcqcq());
        assert_eq!(entry.suffix, DstarSuffix::default());
        Ok(())
    }

    #[tokio::test]
    async fn set_urcall_pads_and_writes_slot_1() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1,KQ4NIT  ,    \r", b"DC 1,KQ4NIT  ,    \r");

        let mut radio = Radio::new(mock);
        radio
            .set_urcall(DstarCallsign::new("KQ4NIT")?, DstarSuffix::default())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn get_rpt1_reads_slot_2() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 2\r", b"DC 2,W4BFB  C,    \r");

        let mut radio = Radio::new(mock);
        let entry = radio.get_rpt1().await?;
        assert_eq!(entry.callsign, DstarCallsign::new("W4BFB  C")?);
        Ok(())
    }

    #[tokio::test]
    async fn set_rpt1_pads_and_writes_slot_2() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 2,W4BFB  C,    \r", b"DC 2,W4BFB  C,    \r");

        let mut radio = Radio::new(mock);
        radio
            .set_rpt1(DstarCallsign::new("W4BFB  C")?, DstarSuffix::default())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn get_rpt2_reads_slot_3() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 3\r", b"DC 3,W4BFB  G,    \r");

        let mut radio = Radio::new(mock);
        let entry = radio.get_rpt2().await?;
        assert_eq!(entry.callsign, DstarCallsign::new("W4BFB  G")?);
        Ok(())
    }

    #[tokio::test]
    async fn set_rpt2_pads_and_writes_slot_3() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 3,W4BFB  G,    \r", b"DC 3,W4BFB  G,    \r");

        let mut radio = Radio::new(mock);
        radio
            .set_rpt2(DstarCallsign::new("W4BFB  G")?, DstarSuffix::default())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn prepare_reflector_link_sets_urcall_with_link_suffix() -> TestResult {
        let mut mock = MockTransport::new();
        // "REF030" padded to 8 = "REF030  ", suffix "CL" padded to 4 = "CL  "
        mock.expect(b"DC 1,REF030  ,CL  \r", b"DC 1,REF030  ,CL  \r");

        let mut radio = Radio::new(mock);
        radio
            .prepare_reflector_link(ReflectorCallsign::try_from_str("REF030")?, Module::C)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn prepare_reflector_unlink_sets_unlink_urcall() -> TestResult {
        let mut mock = MockTransport::new();
        // "       U" is already 8 chars, suffix "" padded to "    "
        mock.expect(b"DC 1,       U,    \r", b"DC 1,       U,    \r");

        let mut radio = Radio::new(mock);
        radio.prepare_reflector_unlink().await?;
        Ok(())
    }

    #[tokio::test]
    async fn set_cq_sets_cqcqcq() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1,CQCQCQ  ,    \r", b"DC 1,CQCQCQ  ,    \r");

        let mut radio = Radio::new(mock);
        radio.set_cq().await?;
        Ok(())
    }

    #[tokio::test]
    async fn route_to_callsign_sets_urcall() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"DC 1,KQ4NIT  ,    \r", b"DC 1,KQ4NIT  ,    \r");

        let mut radio = Radio::new(mock);
        radio
            .route_to_callsign(DstarCallsign::new("KQ4NIT")?)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn reflector_callsign_cannot_inject_a_dc_field() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        let reflector = ReflectorCallsign::try_from_str("REF,30")?;
        let result = radio.prepare_reflector_link(reflector, Module::C).await;
        assert!(matches!(result, Err(Error::Validation(_))));
        Ok(())
    }
}
