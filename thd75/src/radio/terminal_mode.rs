//! Reflector Terminal Mode lifecycle: Menu 985 routing plus Menu 650 mode.
//!
//! Enabling Reflector Terminal Mode is not an ordinary setting write: the
//! Menu 985 and Menu 650 live only in MCP memory, an enabling update reboots
//! the radio, and the rebooted firmware first answers CAT for tens of seconds
//! before the gateway application takes over and the link switches to the
//! MMDVM protocol (hardware-verified: CAT alive around +10 s, dead around
//! +49 s, MMDVM answering afterwards). This module owns that whole transition
//! so applications stop hand-coding raw MCP offsets and reboot polling:
//!
//! - [`Radio::set_reflector_terminal_mode_detached`] performs one qualified,
//!   schema-gated MCP transaction that binds Menu 985 to the caller-selected
//!   host interface and enables Menu 650. Both bytes are read back before the
//!   detached exit; the reboot is expected, so a normal CAT reconnect would
//!   race it. Both offsets are pinned to the generated menu registry by test.
//! - [`Radio::enter_reflector_terminal_mode`] composes the write with the
//!   reboot wait: it polls the same transport identity with MMDVM probes,
//!   reopening between attempts, until terminal mode answers or the window
//!   elapses. On success the returned radio's link is positively proved to
//!   speak MMDVM; hand it to
//!   [`DstarGateway`](crate::dstar_gateway::DstarGateway) or
//!   [`MmdvmSession`](crate::radio::mmdvm_session) entry points, never to
//!   ordinary CAT.
//!
//! Once either terminal-setting write may have reached the radio, this module
//! never hands the handle back for ordinary CAT: the firmware can still switch
//! to MMDVM tens of seconds later, so an early-boot CAT answer proves nothing.
//! Failure paths after that point close the connection and report `None` for
//! the radio.

use std::time::Duration;

use crate::error::Error;
use crate::protocol::programming::{self, WritableMcpPage};
use crate::radio::diagnostics::LinkDiagnosis;
use crate::radio::programming::DetachedMcpPageUpdate;
use crate::transport::Transport;
use crate::types::{DvGatewayMode, PcOutputInterface};

use super::Radio;

/// Generated registry name of the Menu 650 DV gateway mode field.
const GATEWAY_MODE_FIELD_NAME: &str = "dv.DvGatewayModeDvGateway";

/// Generated registry name of Menu 985's DV Gateway interface field.
const GATEWAY_INTERFACE_FIELD_NAME: &str = "radio.DvGatewayInterface";

/// MCP offset of Menu 985's DV Gateway interface byte.
const GATEWAY_INTERFACE_OFFSET: usize = 0x1093;

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

/// MCP page containing the Menu 985 interface byte.
#[expect(
    clippy::cast_possible_truncation,
    reason = "GATEWAY_INTERFACE_OFFSET / PAGE_SIZE is 0x10 and the registry pin test keeps the \
              offset inside the 500 KB image"
)]
const GATEWAY_INTERFACE_PAGE: u16 = (GATEWAY_INTERFACE_OFFSET / programming::PAGE_SIZE) as u16;

/// Byte index of the Menu 985 interface value within its page.
const GATEWAY_INTERFACE_BYTE: usize = GATEWAY_INTERFACE_OFFSET % programming::PAGE_SIZE;

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
    /// Bind Reflector Terminal Mode to one host interface and enable it.
    ///
    /// The connected radio is first proved to be the exact MCP-D75 schema
    /// target ([`Radio::verify_mcp_schema_target`]). Menu 985 is set to
    /// `interface` and Menu 650 is set to Reflector Terminal in one sparse MCP
    /// session. Both pages are read before either is written, changed pages
    /// are verified by read-back, and the session exits detached whenever
    /// either setting changed. If both already match, no write occurs and CAT
    /// is restored normally. Prefer
    /// [`Radio::enter_reflector_terminal_mode`], which owns that wait.
    ///
    /// # Errors
    ///
    /// Returns [`Error::McpUnsupportedSchemaTarget`] before any MCP entry for
    /// an unqualified model or firmware, and MCP entry, page, exit, or
    /// recovery errors from the detached update. The structured error reports
    /// which selector pages may have been written and which were read-back
    /// verified. An ambiguous binary exchange is already closed without an
    /// exit byte and requires a radio power cycle.
    pub async fn set_reflector_terminal_mode_detached(
        &mut self,
        interface: PcOutputInterface,
    ) -> Result<DetachedMcpPageUpdate, Error> {
        self.verify_mcp_schema_target().await?;
        self.set_reflector_terminal_mode_detached_unverified(interface)
            .await
    }

    /// Configure Menu 985 and Menu 650 without repeating schema preflight.
    ///
    /// For callers that have already proved the exact MCP-D75 schema target
    /// on this connection (their own [`Radio::verify_mcp_schema_target`]
    /// call, typically because they also needed the identity for messaging).
    /// Prefer [`Radio::set_reflector_terminal_mode_detached`], which verifies
    /// first.
    ///
    /// # Errors
    ///
    /// MCP entry, page, exit, or recovery errors from the detached update. An
    /// ambiguous binary exchange is already closed without an exit byte and
    /// requires a radio power cycle.
    pub async fn set_reflector_terminal_mode_detached_unverified(
        &mut self,
        interface: PcOutputInterface,
    ) -> Result<DetachedMcpPageUpdate, Error> {
        let interface_value = u8::from(interface);
        let mode_value = u8::from(DvGatewayMode::ReflectorTerminal);
        tracing::info!(
            interface_field = GATEWAY_INTERFACE_FIELD_NAME,
            interface_offset = GATEWAY_INTERFACE_OFFSET,
            interface = %interface,
            mode_field = GATEWAY_MODE_FIELD_NAME,
            mode_offset = GATEWAY_MODE_OFFSET,
            "binding and enabling Reflector Terminal Mode via detached MCP update"
        );
        let interface_page = WritableMcpPage::new(GATEWAY_INTERFACE_PAGE)?;
        let mode_page = WritableMcpPage::new(GATEWAY_MODE_PAGE)?;
        self.modify_memory_pages_detached_if_changed(&[interface_page, mode_page], |page, data| {
            match page.as_raw() {
                GATEWAY_INTERFACE_PAGE => data[GATEWAY_INTERFACE_BYTE] = interface_value,
                GATEWAY_MODE_PAGE => data[GATEWAY_MODE_BYTE] = mode_value,
                _ => unreachable!("only the two terminal-mode pages were requested"),
            }
        })
        .await
        .map_err(Error::from)
    }

    /// Disable DV Gateway mode with a detached, schema-qualified MCP update.
    ///
    /// Menu 985 is deliberately left unchanged because no interface owns the
    /// gateway after Menu 650 is Off. If Menu 650 is already Off, no flash
    /// write occurs and the normal MCP exit restores CAT.
    ///
    /// # Errors
    ///
    /// Returns schema preflight, MCP entry, page, exit, or recovery errors.
    pub async fn disable_dv_gateway_detached(&mut self) -> Result<DetachedMcpPageUpdate, Error> {
        self.verify_mcp_schema_target().await?;
        self.disable_dv_gateway_detached_unverified().await
    }

    /// Disable DV Gateway mode after the caller already proved the schema.
    ///
    /// # Errors
    ///
    /// Returns MCP entry, page, exit, or recovery errors.
    pub async fn disable_dv_gateway_detached_unverified(
        &mut self,
    ) -> Result<DetachedMcpPageUpdate, Error> {
        tracing::info!(
            field = GATEWAY_MODE_FIELD_NAME,
            offset = GATEWAY_MODE_OFFSET,
            "disabling DV gateway mode via detached MCP update"
        );
        let page = WritableMcpPage::new(GATEWAY_MODE_PAGE)?;
        let off = u8::from(DvGatewayMode::Off);
        self.modify_memory_pages_detached_if_changed(&[page], |_, data| {
            data[GATEWAY_MODE_BYTE] = off;
        })
        .await
        .map_err(Error::from)
    }

    /// Put the radio into Reflector Terminal Mode and prove its link speaks
    /// MMDVM.
    ///
    /// Composes the schema preflight, the detached Menu 985 / Menu 650 update, and the
    /// reboot wait: the same transport identity is probed with MMDVM
    /// `GET_VERSION` frames, reopening between attempts, until terminal mode
    /// answers or `transition.window()` elapses. The caller must explicitly
    /// provide the physical host interface it owns; Menu 985 is written and
    /// verified together with Menu 650 before the reboot. Even when both
    /// stored bytes already match, the wait still runs: the MCP exit reset the
    /// radio, and an early-boot CAT answer proves nothing about the mode the
    /// firmware settles into.
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
    ///     .enter_reflector_terminal_mode(
    ///         kenwood_thd75::types::PcOutputInterface::Usb,
    ///         TerminalModeTransition::RECOMMENDED,
    ///     )
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
    /// A completed schema rejection before MCP entry returns the radio
    /// (`Some`) only while its CAT boundary remains proved. A failed or
    /// malformed identity exchange closes the connection and returns `None`,
    /// as does every MCP entry, update, cleanup, or transition failure;
    /// [`Error::TerminalModeNotEngaged`] reports an expired transition window
    /// after both stored selectors were read-back verified.
    pub async fn enter_reflector_terminal_mode(
        mut self,
        interface: PcOutputInterface,
        transition: TerminalModeTransition,
    ) -> Result<Self, (Option<Self>, Error)> {
        if let Err(error) = self.verify_mcp_schema_target().await {
            if self.cat_recovery_required() || error.requires_recovery() {
                drop(self.disconnect().await);
                return Err((None, error));
            }
            return Err((Some(self), error));
        }

        match self
            .set_reflector_terminal_mode_detached_unverified(interface)
            .await
        {
            Ok(DetachedMcpPageUpdate::ChangedRadioRebooting) => {
                tracing::info!(
                    %interface,
                    "Menu 985 and/or Menu 650 changed; radio rebooting into terminal mode"
                );
            }
            Ok(DetachedMcpPageUpdate::UnchangedCatReady) => {
                // The MCP exit still reset the radio; the CAT proof from the
                // unchanged path can be the early boot window only.
                tracing::info!(
                    %interface,
                    "Menu 985 route and Menu 650 mode already matched; waiting for MMDVM after \
                     the MCP reset"
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
    fn registry_pins_both_terminal_mode_offsets() -> TestResult {
        let mode_field = menu_field(GATEWAY_MODE_FIELD_NAME)
            .ok_or("the generated registry must contain the DV gateway mode field")?;
        assert_eq!(
            mode_field.descriptor.offset, GATEWAY_MODE_OFFSET,
            "the local mode offset must match the generated registry"
        );
        let interface_field = menu_field(GATEWAY_INTERFACE_FIELD_NAME)
            .ok_or("the generated registry must contain the DV gateway interface field")?;
        assert_eq!(
            interface_field.descriptor.offset, GATEWAY_INTERFACE_OFFSET,
            "the local interface offset must match the generated registry"
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

    /// Queue the schema preflight plus one two-page detached update that
    /// changes Menu 985 from USB to Bluetooth and Menu 650 from Off to
    /// Reflector Terminal.
    fn queue_changed_gateway_write(mock: &mut MockTransport) -> TestResult {
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let interface_original = [0_u8; programming::PAGE_SIZE];
        let mut interface_modified = interface_original;
        interface_modified[GATEWAY_INTERFACE_BYTE] = u8::from(PcOutputInterface::Bluetooth);
        let mode_original = [0_u8; programming::PAGE_SIZE];
        let mut mode_modified = mode_original;
        mode_modified[GATEWAY_MODE_BYTE] = 1;

        let interface_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect(
            &interface_read,
            &build_w_response(GATEWAY_INTERFACE_PAGE, &interface_original),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mode_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(
            &mode_read,
            &build_w_response(GATEWAY_MODE_PAGE, &mode_original),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let interface_write = programming::build_write_command(
            WritableMcpPage::new(GATEWAY_INTERFACE_PAGE)?,
            &interface_modified,
        );
        mock.expect(&interface_write, &[programming::ACK]);
        mock.expect(
            &interface_read,
            &build_w_response(GATEWAY_INTERFACE_PAGE, &interface_modified),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mode_write = programming::build_write_command(
            WritableMcpPage::new(GATEWAY_MODE_PAGE)?,
            &mode_modified,
        );
        mock.expect(&mode_write, &[programming::ACK]);
        mock.expect(
            &mode_read,
            &build_w_response(GATEWAY_MODE_PAGE, &mode_modified),
        );
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
    async fn enter_binds_menu_985_enables_menu_650_then_proves_mmdvm() -> TestResult {
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
        let result = radio
            .enter_reflector_terminal_mode(PcOutputInterface::Bluetooth, transition)
            .await;
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
        let result = radio
            .enter_reflector_terminal_mode(PcOutputInterface::Bluetooth, transition)
            .await;
        let Err((returned, error)) = result else {
            return Err("an unanswered transition window must fail".into());
        };
        assert!(
            returned.is_none(),
            "after the terminal-mode update the radio must never be handed back for CAT"
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
            .enter_reflector_terminal_mode(
                PcOutputInterface::Bluetooth,
                TerminalModeTransition::RECOMMENDED,
            )
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

    #[tokio::test(start_paused = true)]
    async fn schema_identity_timeout_closes_instead_of_returning_poisoned_cat() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"ID\r");
        let radio = Radio::new(mock);

        let result = radio
            .enter_reflector_terminal_mode(
                PcOutputInterface::Bluetooth,
                TerminalModeTransition::RECOMMENDED,
            )
            .await;
        let Err((returned, error)) = result else {
            return Err("an unanswered identity exchange must fail".into());
        };
        assert!(
            returned.is_none(),
            "a timed-out CAT identity exchange must not return a poisoned handle"
        );
        assert!(error.is_link_lost());
        assert!(error.requires_recovery());
        Ok(())
    }

    #[tokio::test]
    async fn unverified_detached_setter_skips_the_identity_exchanges() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let mut interface = [0_u8; programming::PAGE_SIZE];
        interface[GATEWAY_INTERFACE_BYTE] = u8::from(PcOutputInterface::Bluetooth);
        let interface_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect(
            &interface_read,
            &build_w_response(GATEWAY_INTERFACE_PAGE, &interface),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mut mode = [0_u8; programming::PAGE_SIZE];
        mode[GATEWAY_MODE_BYTE] = 1;
        let mode_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&mode_read, &build_w_response(GATEWAY_MODE_PAGE, &mode));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let update = radio
            .set_reflector_terminal_mode_detached_unverified(PcOutputInterface::Bluetooth)
            .await?;
        assert!(
            matches!(update, DetachedMcpPageUpdate::UnchangedCatReady),
            "the unverified setter must go straight to MCP: {update:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn qualified_setter_skips_writes_when_both_bytes_match() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);
        let interface = [0_u8; programming::PAGE_SIZE];
        let interface_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect(
            &interface_read,
            &build_w_response(GATEWAY_INTERFACE_PAGE, &interface),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mut mode = [0_u8; programming::PAGE_SIZE];
        mode[GATEWAY_MODE_BYTE] = 1;
        let mode_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&mode_read, &build_w_response(GATEWAY_MODE_PAGE, &mode));
        mock.expect(&[programming::ACK], &[programming::ACK]);
        // Unchanged: normal exit with CAT restoration.
        mock.expect(&[programming::EXIT], &[programming::ACK]);
        mock.expect_reopen(Ok(()));
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);

        let update = radio
            .set_reflector_terminal_mode_detached(PcOutputInterface::Usb)
            .await?;
        assert!(
            matches!(update, DetachedMcpPageUpdate::UnchangedCatReady),
            "a matching byte must skip the write: {update:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn qualified_setter_corrects_route_when_mode_is_already_enabled() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        mock.expect(programming::ENTER_PROGRAMMING, programming::ENTER_RESPONSE);

        let interface_original = [0_u8; programming::PAGE_SIZE];
        let mut interface_modified = interface_original;
        interface_modified[GATEWAY_INTERFACE_BYTE] = u8::from(PcOutputInterface::Bluetooth);
        let mut mode = [0_u8; programming::PAGE_SIZE];
        mode[GATEWAY_MODE_BYTE] = 1;
        let interface_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_INTERFACE_PAGE)?);
        mock.expect(
            &interface_read,
            &build_w_response(GATEWAY_INTERFACE_PAGE, &interface_original),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        let mode_read =
            programming::build_read_command(programming::McpPage::new(GATEWAY_MODE_PAGE)?);
        mock.expect(&mode_read, &build_w_response(GATEWAY_MODE_PAGE, &mode));
        mock.expect(&[programming::ACK], &[programming::ACK]);

        let interface_write = programming::build_write_command(
            WritableMcpPage::new(GATEWAY_INTERFACE_PAGE)?,
            &interface_modified,
        );
        mock.expect(&interface_write, &[programming::ACK]);
        mock.expect(
            &interface_read,
            &build_w_response(GATEWAY_INTERFACE_PAGE, &interface_modified),
        );
        mock.expect(&[programming::ACK], &[programming::ACK]);
        mock.expect(&[programming::EXIT], &[programming::ACK]);

        let mut radio = Radio::new(mock);
        let update = radio
            .set_reflector_terminal_mode_detached(PcOutputInterface::Bluetooth)
            .await?;
        assert!(
            matches!(update, DetachedMcpPageUpdate::ChangedRadioRebooting),
            "a route-only correction must use the detached reboot path: {update:?}"
        );
        radio.transport.assert_complete();
        Ok(())
    }
}
