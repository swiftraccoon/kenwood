//! Reflector Terminal Mode lifecycle: the Menu 650 DV gateway transition.
//!
//! Enabling Reflector Terminal Mode is not an ordinary setting write: the
//! Menu 650 byte lives only in MCP memory, the write reboots the radio, and
//! the rebooted firmware first answers CAT for tens of seconds before the
//! gateway application takes over and the link switches to the MMDVM
//! protocol (hardware-verified: CAT alive around +10 s, dead around +49 s,
//! MMDVM answering afterwards). This module owns that whole transition so
//! applications stop hand-coding raw MCP offsets and reboot polling:
//!
//! - [`Radio::set_dv_gateway_mode_detached`] performs the qualified,
//!   schema-gated Menu 650 write with the detached MCP exit (the reboot is
//!   expected; a normal exit's CAT reconnect would race it). The byte offset
//!   is pinned to the generated menu registry by test.
//! - [`Radio::enter_reflector_terminal_mode`] composes the write with the
//!   reboot wait: it polls the same transport identity with MMDVM probes,
//!   reopening between attempts, until terminal mode answers or the window
//!   elapses. On success the returned radio's link is positively proved to
//!   speak MMDVM; hand it to
//!   [`DstarGateway`](crate::dstar_gateway::DstarGateway) or
//!   [`MmdvmSession`](crate::radio::mmdvm_session) entry points, never to
//!   ordinary CAT.
//!
//! Once the Menu 650 write may have reached the radio, this module never
//! hands the handle back for ordinary CAT: the firmware can still switch to
//! MMDVM tens of seconds later, so an early-boot CAT answer proves nothing.
//! Failure paths after that point close the connection and report `None` for
//! the radio.

use std::time::Duration;

use crate::error::Error;
use crate::protocol::programming::{self, WritableMcpPage};
use crate::radio::diagnostics::LinkDiagnosis;
use crate::radio::programming::DetachedMcpPageUpdate;
use crate::transport::Transport;
use crate::types::DvGatewayMode;

use super::Radio;

/// Generated registry name of the Menu 650 DV gateway mode field.
const GATEWAY_MODE_FIELD_NAME: &str = "dv.DvGatewayModeDvGateway";

/// MCP offset of the Menu 650 DV gateway mode byte.
///
/// Kept equal to the generated registry entry for
/// [`GATEWAY_MODE_FIELD_NAME`]; the `registry_pins_the_gateway_offset` test
/// enforces the equality so a regenerated registry cannot silently diverge.
const GATEWAY_MODE_OFFSET: usize = 0x1CA0;

/// MCP page containing the gateway mode byte.
#[expect(
    clippy::cast_possible_truncation,
    reason = "GATEWAY_MODE_OFFSET / PAGE_SIZE is 0x1C, far inside the u16 page space; the \
              registry pin test keeps the offset inside the 500 KB image"
)]
const GATEWAY_MODE_PAGE: u16 = (GATEWAY_MODE_OFFSET / programming::PAGE_SIZE) as u16;

/// Byte index of the gateway mode value within its page.
const GATEWAY_MODE_BYTE: usize = GATEWAY_MODE_OFFSET % programming::PAGE_SIZE;

/// Why a terminal-mode transition policy could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TerminalModeTransitionError {
    /// The poll interval is zero or longer than the transition window.
    #[error(
        "terminal-mode poll interval {poll_interval:?} must be nonzero and no longer than the \
         transition window {window:?}"
    )]
    InvalidPollInterval {
        /// Requested poll interval.
        poll_interval: Duration,
        /// Requested transition window.
        window: Duration,
    },
}

/// Timing policy for the reboot-to-MMDVM transition wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalModeTransition {
    window: Duration,
    poll_interval: Duration,
}

impl TerminalModeTransition {
    /// Hardware-verified default: terminal mode engages slowly after the
    /// reboot (CAT can answer for tens of seconds first), so probe every
    /// three seconds for up to ninety seconds.
    pub const RECOMMENDED: Self = Self {
        window: Duration::from_secs(90),
        poll_interval: Duration::from_secs(3),
    };

    /// Build a custom transition policy.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalModeTransitionError::InvalidPollInterval`] when the
    /// poll interval is zero or exceeds the window.
    pub const fn new(
        window: Duration,
        poll_interval: Duration,
    ) -> Result<Self, TerminalModeTransitionError> {
        if poll_interval.is_zero() || poll_interval.as_nanos() > window.as_nanos() {
            return Err(TerminalModeTransitionError::InvalidPollInterval {
                poll_interval,
                window,
            });
        }
        Ok(Self {
            window,
            poll_interval,
        })
    }

    /// Total time allowed for the radio to start answering MMDVM.
    #[must_use]
    pub const fn window(self) -> Duration {
        self.window
    }

    /// Delay between MMDVM probes.
    #[must_use]
    pub const fn poll_interval(self) -> Duration {
        self.poll_interval
    }
}

impl<T: Transport> Radio<T> {
    /// Write the Menu 650 DV gateway mode with the detached MCP exit.
    ///
    /// The connected radio is first proved to be the exact MCP-D75 schema
    /// target ([`Radio::verify_mcp_schema_target`]). The write itself uses
    /// the detached page update: when the stored byte already matches, no
    /// write happens and MCP exits normally; when it changes, the radio
    /// reboots into the requested mode and this connection's CAT framing is
    /// gone until the caller handles the transition. Prefer
    /// [`Radio::enter_reflector_terminal_mode`], which owns that wait.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpUnsupportedSchemaTarget`] before any MCP entry for
    /// an unqualified model or firmware, and MCP entry, page, exit, or
    /// recovery errors from the detached update. After an error the normal
    /// MCP recovery rules apply
    /// ([`Radio::recover_from_interrupted_mcp`], reconnect).
    pub async fn set_dv_gateway_mode_detached(
        &mut self,
        mode: DvGatewayMode,
    ) -> Result<DetachedMcpPageUpdate, Error> {
        self.verify_mcp_schema_target().await?;
        self.set_dv_gateway_mode_detached_unverified(mode).await
    }

    /// The detached Menu 650 write without the schema preflight.
    ///
    /// For callers that have already proved the exact MCP-D75 schema target
    /// on this connection (their own [`Radio::verify_mcp_schema_target`]
    /// call, typically because they also needed the identity for messaging).
    /// Prefer [`Radio::set_dv_gateway_mode_detached`], which verifies first.
    ///
    /// # Errors
    ///
    /// MCP entry, page, exit, or recovery errors from the detached update;
    /// after an error the normal MCP recovery rules apply.
    pub async fn set_dv_gateway_mode_detached_unverified(
        &mut self,
        mode: DvGatewayMode,
    ) -> Result<DetachedMcpPageUpdate, Error> {
        let value = match mode {
            DvGatewayMode::Off => 0_u8,
            DvGatewayMode::ReflectorTerminal => 1,
        };
        tracing::info!(
            field = GATEWAY_MODE_FIELD_NAME,
            offset = GATEWAY_MODE_OFFSET,
            value,
            "writing DV gateway mode via detached MCP update"
        );
        let page = WritableMcpPage::new(GATEWAY_MODE_PAGE)?;
        self.modify_memory_page_detached_if_changed(page, |data| {
            data[GATEWAY_MODE_BYTE] = value;
        })
        .await
    }

    /// Put the radio into Reflector Terminal Mode and prove its link speaks
    /// MMDVM.
    ///
    /// Composes the schema preflight, the detached Menu 650 write, and the
    /// reboot wait: the same transport identity is probed with MMDVM
    /// `GET_VERSION` frames, reopening between attempts, until terminal mode
    /// answers or `transition.window()` elapses. Even when the stored byte
    /// was already Reflector Terminal, the wait still runs: the MCP exit
    /// reset the radio, and an early-boot CAT answer proves nothing about
    /// the mode the firmware settles into.
    ///
    /// On success the returned radio is positively proved to speak MMDVM.
    /// Do not issue ordinary CAT on it.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use kenwood_thd75::radio::Radio;
    /// # use kenwood_thd75::transport::SerialTransport;
    /// # use kenwood_thd75::TerminalModeTransition;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = SerialTransport::open("/dev/cu.usbmodem1234")?;
    /// let radio = Radio::connect_with_tnc_exit(transport).await?;
    ///
    /// match radio
    ///     .enter_reflector_terminal_mode(TerminalModeTransition::RECOMMENDED)
    ///     .await
    /// {
    ///     Ok(radio) => { /* hand the MMDVM-proved radio to DstarGateway */ drop(radio); }
    ///     Err((Some(radio), error)) => {
    ///         // Preflight failure: the radio is still usable for CAT.
    ///         drop(radio.disconnect().await);
    ///         return Err(error.into());
    ///     }
    ///     Err((None, error)) => return Err(error.into()),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// A failure before the Menu 650 write can have reached the radio
    /// returns the radio back (`Some`), still usable for CAT. Once the write
    /// may have landed, failure paths close the connection and return `None`
    /// with the underlying error;
    /// [`Error::TerminalModeNotEngaged`] reports an expired transition
    /// window, including the Menu 985 interface-binding guidance.
    pub async fn enter_reflector_terminal_mode(
        mut self,
        transition: TerminalModeTransition,
    ) -> Result<Self, (Option<Self>, Error)> {
        if let Err(error) = self.verify_mcp_schema_target().await {
            return Err((Some(self), error));
        }

        match self
            .set_dv_gateway_mode_detached_unverified(DvGatewayMode::ReflectorTerminal)
            .await
        {
            Ok(DetachedMcpPageUpdate::ChangedRadioRebooting) => {
                tracing::info!("Menu 650 changed; radio rebooting into terminal mode");
            }
            Ok(DetachedMcpPageUpdate::UnchangedCatReady) => {
                // The MCP exit still reset the radio; the CAT proof from the
                // unchanged path can be the early boot window only.
                tracing::info!(
                    "Menu 650 already Reflector Terminal; waiting for MMDVM after the MCP reset"
                );
            }
            Err(error) => {
                // The page operation may have landed even though it errored.
                // Never hand this handle back for ordinary CAT.
                drop(self.disconnect().await);
                return Err((None, error));
            }
        }

        let deadline = tokio::time::Instant::now() + transition.window();
        loop {
            // Inner scope: the pinned step future borrows `self`, and the
            // borrow must end before an outcome arm can move `self` out.
            let outcome = {
                let step = async {
                    tokio::time::sleep(transition.poll_interval()).await;
                    if self.probe_silent_link().await == LinkDiagnosis::MmdvmMode {
                        return true;
                    }
                    if let Err(error) = self.reopen_for_link_diagnosis().await {
                        tracing::debug!(%error, "terminal-mode transport not ready to reopen");
                    }
                    false
                };
                tokio::pin!(step);
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => None,
                    engaged = &mut step => Some(engaged),
                }
            };
            match outcome {
                Some(true) => {
                    tracing::info!("radio is in Reflector Terminal Mode");
                    return Ok(self);
                }
                Some(false) => {}
                None => break,
            }
        }

        drop(self.disconnect().await);
        Err((
            None,
            Error::TerminalModeNotEngaged {
                window: transition.window(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::menu_fields::menu_field;
    use crate::transport::MockTransport;
    use mmdvm_core::{MMDVM_FRAME_START, MMDVM_GET_VERSION};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The same probe bytes the diagnostics module sends.
    const PROBE: [u8; 3] = [MMDVM_FRAME_START, 0x03, MMDVM_GET_VERSION];

    #[test]
    fn registry_pins_the_gateway_offset() -> TestResult {
        let field = menu_field(GATEWAY_MODE_FIELD_NAME)
            .ok_or("the generated registry must contain the DV gateway mode field")?;
        assert_eq!(
            field.descriptor.offset, GATEWAY_MODE_OFFSET,
            "the local offset constant must match the generated registry"
        );
        Ok(())
    }

    #[test]
    fn transition_policy_rejects_unusable_poll_intervals() {
        let zero = TerminalModeTransition::new(Duration::from_secs(10), Duration::ZERO);
        assert!(matches!(
            zero,
            Err(TerminalModeTransitionError::InvalidPollInterval { .. })
        ));
        let longer = TerminalModeTransition::new(Duration::from_secs(1), Duration::from_secs(2));
        assert!(matches!(
            longer,
            Err(TerminalModeTransitionError::InvalidPollInterval { .. })
        ));
        assert!(
            TerminalModeTransition::new(Duration::from_secs(2), Duration::from_secs(1)).is_ok()
        );
    }

    /// Queue the schema preflight plus the changed-byte detached MCP write:
    /// enter, read (byte 0), verified write of byte 1, verification
    /// read-back, detached exit ACK.
    fn queue_changed_gateway_write(mock: &mut MockTransport) -> TestResult {
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let page_raw = GATEWAY_MODE_PAGE;
        let original = [0_u8; programming::PAGE_SIZE];
        let mut modified = original;
        modified[GATEWAY_MODE_BYTE] = 1;

        let read = programming::build_read_command(programming::McpPage::new(page_raw)?);
        mock.expect(&read, &build_w_response(page_raw, &original));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let write = programming::build_write_command(WritableMcpPage::new(page_raw)?, &modified);
        mock.expect(&write, &[programming::ACK]);
        mock.expect(&read, &build_w_response(page_raw, &modified));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        mock.expect(&[programming::EXIT], &[programming::ACK]);
        Ok(())
    }

    /// Build one MCP `W` response frame for a full page.
    fn build_w_response(page: u16, data: &[u8; programming::PAGE_SIZE]) -> Vec<u8> {
        let [addr_hi, addr_lo] = page.to_be_bytes();
        let mut response = vec![b'W', addr_hi, addr_lo, 0x00, 0x00];
        response.extend_from_slice(data);
        response
    }

    #[tokio::test(start_paused = true)]
    async fn enter_writes_menu_650_then_proves_mmdvm() -> TestResult {
        let mut mock = MockTransport::new();
        queue_changed_gateway_write(&mut mock)?;
        // First probe: no answer; the transport is reopened. Second probe:
        // terminal mode answers.
        mock.expect(&PROBE, b"");
        mock.expect_reopen(Ok(()));
        mock.expect(&PROBE, b"\xE0\x12\x00\x01TH-D75 RTM1.00");
        let radio = Radio::new(mock);

        let transition =
            TerminalModeTransition::new(Duration::from_secs(30), Duration::from_secs(1))?;
        let result = radio.enter_reflector_terminal_mode(transition).await;
        let Ok(radio) = result else {
            return Err(format!("terminal-mode entry must succeed: {result:?}").into());
        };
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn enter_times_out_and_never_returns_the_radio() -> TestResult {
        let mut mock = MockTransport::new();
        queue_changed_gateway_write(&mut mock)?;
        // Every probe inside the two-poll window goes unanswered.
        mock.expect(&PROBE, b"");
        mock.expect_reopen(Ok(()));
        mock.expect(&PROBE, b"");
        mock.expect_reopen(Ok(()));
        let radio = Radio::new(mock);

        let transition =
            TerminalModeTransition::new(Duration::from_secs(2), Duration::from_secs(1))?;
        let result = radio.enter_reflector_terminal_mode(transition).await;
        let Err((returned, error)) = result else {
            return Err("an unanswered transition window must fail".into());
        };
        assert!(
            returned.is_none(),
            "after the Menu 650 write the radio must never be handed back for CAT"
        );
        assert!(
            matches!(error, Error::TerminalModeNotEngaged { window } if window == transition.window()),
            "unexpected transition error: {error:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn unqualified_schema_returns_the_radio_untouched() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.04\r");
        let radio = Radio::new(mock);

        let result = radio
            .enter_reflector_terminal_mode(TerminalModeTransition::RECOMMENDED)
            .await;
        let Err((returned, error)) = result else {
            return Err("an unqualified schema target must be refused".into());
        };
        let radio = returned.ok_or("a preflight failure must return the radio")?;
        assert!(
            matches!(error, Error::McpUnsupportedSchemaTarget { .. }),
            "unexpected preflight error: {error:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn unverified_detached_setter_skips_the_identity_exchanges() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let mut stored = [0_u8; programming::PAGE_SIZE];
        stored[GATEWAY_MODE_BYTE] = 1;
        let read = programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read, &build_w_response(GATEWAY_MODE_PAGE, &stored));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let update = radio
            .set_dv_gateway_mode_detached_unverified(DvGatewayMode::ReflectorTerminal)
            .await?;
        assert!(
            matches!(update, DetachedMcpPageUpdate::UnchangedCatReady),
            "the unverified setter must go straight to MCP: {update:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn detached_setter_skips_the_write_when_the_byte_matches() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let mut stored = [0_u8; programming::PAGE_SIZE];
        stored[GATEWAY_MODE_BYTE] = 1;
        let read = programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&read, &build_w_response(GATEWAY_MODE_PAGE, &stored));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        // Unchanged: normal exit with CAT restoration.
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let update = radio
            .set_dv_gateway_mode_detached(DvGatewayMode::ReflectorTerminal)
            .await?;
        assert!(
            matches!(update, DetachedMcpPageUpdate::UnchangedCatReady),
            "a matching byte must skip the write: {update:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }
}
