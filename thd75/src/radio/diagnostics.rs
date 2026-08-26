//! Connection diagnostics: explaining *why* a link is not carrying CAT.
//!
//! When a freshly-opened transport accepts bytes but the radio never
//! answers a CAT command, the link is physically fine yet logically
//! unusable. The TH-D75 does this whenever it is in a DV Gateway mode
//! (Reflector Terminal or Access Point): the firmware swaps its CAT
//! command parser for the MMDVM data protocol, so `ID\r` and every
//! other CAT command is silently ignored.
//!
//! [`Radio::probe_silent_link`] tells the cases apart by probing with an
//! MMDVM frame rather than guessing from a CAT timeout. A reply to an
//! MMDVM `GET_VERSION` is positive proof of MMDVM mode, whereas a CAT
//! timeout alone cannot distinguish "wrong mode" from "dead cable".

use std::time::Duration;

use crate::transport::Transport;
use mmdvm_core::{MMDVM_FRAME_START, MMDVM_GET_VERSION, VersionResponse, decode_frame};

use super::{BinaryProtocolProof, CatState, LinkState, McpPhase, Radio};

/// MMDVM `GET_VERSION` request: sync byte `0xE0`, length `0x03`, type `0x00`.
///
/// A radio in a DV Gateway mode answers this with an `0xE0`-framed
/// version reply; a radio in any other state ignores it.
const MMDVM_GET_VERSION_PROBE: [u8; 3] = [MMDVM_FRAME_START, 0x03, MMDVM_GET_VERSION];

/// How long to wait for an MMDVM reply before concluding the link is
/// unresponsive. MMDVM answers in roughly 20 ms; this is generous.
const MMDVM_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a TH-D75 link is not carrying CAT control traffic.
///
/// Produced by [`Radio::probe_silent_link`], which is meant to be called
/// only after a CAT command has already failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDiagnosis {
    /// A strict GM memory-read or MCP exchange remains unresolved. No
    /// diagnostic probe was sent because only the operation-specific recovery
    /// can establish a clean stream.
    ReconnectRequired,
    /// The radio answered an MMDVM probe: it is in a DV Gateway mode
    /// (Reflector Terminal or Access Point) and speaks the MMDVM data
    /// protocol over this link, so CAT control is unavailable until
    /// DV Gateway mode is switched off on the radio.
    MmdvmMode,
    /// The radio answered neither CAT nor an MMDVM probe. The transport
    /// opened, but nothing the library recognises replied: typically a
    /// cabling or power problem, the wrong serial device, another
    /// program holding the port, or KISS routing pointed elsewhere.
    Unresponsive,
}

impl LinkDiagnosis {
    /// Operator-facing guidance for restoring CAT control.
    ///
    /// Returns plain, newline-separated prose suitable for printing
    /// straight to a terminal.
    #[must_use]
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::ReconnectRequired => {
                "\
An incomplete GM memory-read or MCP exchange left the command stream untrusted.\n\
\n\
Reconnect the transport before sending any more commands."
            }
            Self::MmdvmMode => {
                "\
The radio is in Reflector Terminal / DV Gateway Mode: it is speaking\n\
the MMDVM data protocol over this link, so CAT control commands are\n\
ignored.\n\
\n\
To restore CAT control, on the radio:\n\
  - Open Menu No. 650 (DV Gateway) and select [Off].\n\
  - The green \"TERM\" indicator clears once it is off."
            }
            Self::Unresponsive => {
                "\
The radio did not respond to CAT control or to an MMDVM probe.\n\
\n\
Check that:\n\
  - The USB-C cable is seated, or the Bluetooth radio is paired.\n\
  - The radio is powered on and awake.\n\
  - No other application (BlueDV, a hotspot client) holds the port.\n\
  - KISS TNC routing (Menu No. 983) is not pointed at this port."
            }
        }
    }
}

impl<T: Transport> Radio<T> {
    /// Probe an already-open link to find out why CAT control is silent.
    ///
    /// Call this only after a CAT command (e.g. [`identify`](Radio::identify))
    /// has already failed. It sends an MMDVM `GET_VERSION` frame and
    /// classifies the reply.
    ///
    /// This never returns an error. A poisoned GM stream or unresolved MCP
    /// exchange yields [`LinkDiagnosis::ReconnectRequired`] without sending a
    /// probe. A probe that cannot be written, or that draws no reply, yields
    /// [`LinkDiagnosis::Unresponsive`].
    ///
    /// The raw probe deliberately arms CAT recovery before its first write.
    /// A positive MMDVM response proves a new binary-protocol boundary and
    /// makes the radio safe to consume into an MMDVM session. Every other
    /// result leaves ordinary CAT blocked until reconnect or explicit CAT
    /// restoration. This also permits diagnosis after the failed CAT command
    /// has marked its request/response boundary recovery-required.
    pub async fn probe_silent_link(&mut self) -> LinkDiagnosis {
        if self.require_unpoisoned_gm_stream().is_err()
            || self.mcp_phase != McpPhase::Inactive
            || self.mcp_saved_timeout.is_some()
            || self.mcp_pending_exit_error.is_some()
        {
            tracing::warn!("refusing link probe across an unresolved protocol boundary");
            return LinkDiagnosis::ReconnectRequired;
        }

        // This is raw, non-CAT traffic. Set the state before the first await so
        // cancellation, a partial reply, or a silent CAT parser cannot leave a
        // controller that appears safe for another CAT command.
        self.cat_state = CatState::RecoveryRequired;
        self.desynced = true;
        self.codec.clear();
        let _previous = self.link_state_tx.send_replace(LinkState::Down);

        tracing::info!("probing silent link for MMDVM mode");
        let deadline = tokio::time::Instant::now() + MMDVM_PROBE_TIMEOUT;
        let write =
            tokio::time::timeout_at(deadline, self.transport.write(&MMDVM_GET_VERSION_PROBE)).await;
        if !matches!(write, Ok(Ok(()))) {
            return LinkDiagnosis::Unresponsive;
        }
        let diagnosis = if read_mmdvm_version_response(&mut self.transport, deadline).await {
            LinkDiagnosis::MmdvmMode
        } else {
            LinkDiagnosis::Unresponsive
        };
        if diagnosis == LinkDiagnosis::MmdvmMode {
            // A complete MMDVM frame proves that CAT is no longer the active
            // parser, so no timed-out CAT response can arrive later. Mark the
            // binary boundary synchronously before handing ownership to a
            // typed MMDVM session; ordinary CAT must remain blocked.
            self.codec.clear();
            self.last_cmd_time = None;
            self.desynced = false;
            self.cat_state = CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None });
        }
        tracing::info!(?diagnosis, "link diagnosis complete");
        diagnosis
    }

    /// Reopen the same transport identity without sending or proving CAT.
    ///
    /// This is for a known external protocol transition, such as the reboot
    /// from ordinary firmware into persistent Reflector Terminal Mode. The
    /// radio can reset its USB or Bluetooth interface after an earlier handle
    /// was opened, while CAT is deliberately unavailable on the replacement
    /// link. The reopened controller remains blocked for ordinary CAT and may
    /// only be passed to [`probe_silent_link`](Self::probe_silent_link) until a binary
    /// protocol is proved.
    ///
    /// # Errors
    ///
    /// Returns the unresolved GM or MCP error without touching the transport,
    /// or a transport error when reopening the same identity fails.
    pub async fn reopen_for_link_diagnosis(&mut self) -> Result<(), crate::Error> {
        self.require_unpoisoned_gm_stream()?;
        if self.mcp_phase != McpPhase::Inactive
            || self.mcp_saved_timeout.is_some()
            || self.mcp_pending_exit_error.is_some()
        {
            return Err(crate::Error::McpInterrupted);
        }

        self.cat_state = CatState::RecoveryRequired;
        self.desynced = true;
        self.codec.clear();
        self.last_cmd_time = None;
        self.firmware_version = None;
        self.tuning_mode_a = None;
        self.tuning_mode_b = None;
        let _previous = self.link_state_tx.send_replace(LinkState::Down);

        if let Err(error) = self.close_transport().await {
            tracing::debug!(%error, "close before diagnostic reopen failed");
        }
        self.transport
            .reopen()
            .await
            .map_err(crate::Error::Transport)?;
        self.codec.clear();
        Ok(())
    }
}

/// Read one complete MMDVM `GET_VERSION` response without consuming bytes
/// beyond its advertised frame length.
///
/// Transport reads may split a frame at any byte. Reading the start and length
/// fields first lets the final read be bounded to the exact remaining length,
/// so the asynchronous MMDVM session that takes ownership next cannot inherit
/// the tail of this probe response. Non-frame bytes are skipped while looking
/// for the next MMDVM sync byte, but only a complete, codec-validated
/// `GET_VERSION` frame is accepted as proof.
async fn read_mmdvm_version_response<T: Transport>(
    transport: &mut T,
    deadline: tokio::time::Instant,
) -> bool {
    loop {
        let mut start = [0_u8; 1];
        if !read_exact_until(transport, &mut start, deadline).await {
            return false;
        }
        if start[0] != MMDVM_FRAME_START {
            continue;
        }

        let mut length = [0_u8; 1];
        if !read_exact_until(transport, &mut length, deadline).await {
            return false;
        }

        let mut wire = vec![MMDVM_FRAME_START, length[0]];
        let frame_len = if length[0] == 0 {
            let mut extended_length = [0_u8; 1];
            if !read_exact_until(transport, &mut extended_length, deadline).await {
                return false;
            }
            wire.push(extended_length[0]);
            usize::from(extended_length[0]) + 255
        } else {
            let frame_len = usize::from(length[0]);
            if frame_len < usize::from(mmdvm_core::MIN_FRAME_LEN) {
                continue;
            }
            frame_len
        };

        let already_read = wire.len();
        wire.resize(frame_len, 0);
        let Some(remainder) = wire.get_mut(already_read..) else {
            return false;
        };
        if !read_exact_until(transport, remainder, deadline).await {
            return false;
        }

        match decode_frame(&wire) {
            Ok(Some((frame, consumed))) if consumed == wire.len() => {
                if frame.command == MMDVM_GET_VERSION
                    && VersionResponse::parse(&frame.payload).is_ok_and(|version| {
                        matches!(version.protocol, 1 | 2) && !version.description.is_empty()
                    })
                {
                    return true;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return false,
        }
    }
}

/// Fill one bounded slice before the shared absolute deadline.
async fn read_exact_until<T: Transport>(
    transport: &mut T,
    target: &mut [u8],
    deadline: tokio::time::Instant,
) -> bool {
    let mut filled = 0;
    while filled < target.len() {
        let Some(unfilled) = target.get_mut(filled..) else {
            return false;
        };
        let remaining = unfilled.len();
        let read = tokio::time::timeout_at(deadline, transport.read(unfilled)).await;
        let count = match read {
            Ok(Ok(count)) if count > 0 && count <= remaining => count,
            _ => return false,
        };
        filled += count;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn probe_silent_link_detects_mmdvm_mode() -> TestResult {
        let mut mock = MockTransport::new();
        // The MMDVM GET_VERSION probe draws an 0xE0-framed version reply.
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM 2018");
        let mut radio = Radio::new(mock);
        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::MmdvmMode);
        assert!(
            matches!(
                radio.require_cat_ready(),
                Err(crate::Error::CatRecoveryRequired)
            ),
            "positive MMDVM proof must continue blocking ordinary CAT"
        );
        assert_eq!(
            radio.cat_state,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        let (transport, _restore) = radio
            .into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
            .map_err(|(_, error)| error)?;
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn probe_silent_link_rejects_echoed_get_version_request() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(&MMDVM_GET_VERSION_PROBE, &MMDVM_GET_VERSION_PROBE);
        let mut radio = Radio::new(mock);

        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::Unresponsive);
        assert!(matches!(
            radio.require_cat_ready(),
            Err(crate::Error::CatRecoveryRequired)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn probe_silent_link_leaves_a_coalesced_following_frame_unread() -> TestResult {
        let mut response = b"\xE0\x0E\x00\x01MMDVM 2018".to_vec();
        response.extend_from_slice(b"\xE0\x03\x01");
        let mut mock = MockTransport::new();
        mock.expect(&MMDVM_GET_VERSION_PROBE, &response);
        let mut radio = Radio::new(mock);

        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::MmdvmMode);
        let mut trailing = [0_u8; 3];
        let count = radio.transport.read(&mut trailing).await?;
        assert_eq!(count, trailing.len());
        assert_eq!(trailing, [MMDVM_FRAME_START, 0x03, 0x01]);
        Ok(())
    }

    #[tokio::test]
    async fn probe_silent_link_reports_unresponsive_on_non_mmdvm_reply() -> TestResult {
        let mut mock = MockTransport::new();
        // A reply that is not 0xE0-framed is not an MMDVM modem.
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"?\r");
        let mut radio = Radio::new(mock);
        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::Unresponsive);
        assert!(
            matches!(
                radio.require_cat_ready(),
                Err(crate::Error::CatRecoveryRequired)
            ),
            "a non-MMDVM raw reply must block subsequent CAT traffic"
        );
        Ok(())
    }

    #[tokio::test]
    async fn probe_silent_link_reports_unresponsive_on_empty_reply() -> TestResult {
        let mut mock = MockTransport::new();
        // No bytes back at all, so nothing recognisable answered.
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"");
        let mut radio = Radio::new(mock);
        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::Unresponsive);
        assert!(
            matches!(
                radio.require_cat_ready(),
                Err(crate::Error::CatRecoveryRequired)
            ),
            "a silent raw probe must block subsequent CAT traffic"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_diagnosis_during_read_keeps_cat_blocked() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(&MMDVM_GET_VERSION_PROBE);
        let mut radio = Radio::new(mock);

        let cancelled =
            tokio::time::timeout(Duration::from_millis(1), radio.probe_silent_link()).await;
        assert!(cancelled.is_err(), "diagnostic read unexpectedly completed");
        assert!(matches!(
            radio.require_cat_ready(),
            Err(crate::Error::CatRecoveryRequired)
        ));
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn probe_silent_link_rejects_truncated_mmdvm_frame() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_partial_then_hang(&MMDVM_GET_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM");
        let mut radio = Radio::new(mock);

        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::Unresponsive);
        assert!(matches!(
            radio.require_cat_ready(),
            Err(crate::Error::CatRecoveryRequired)
        ));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn probe_silent_link_can_prove_mmdvm_after_cat_timeout() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"ID\r");
        mock.expect_reads(
            &MMDVM_GET_VERSION_PROBE,
            &[b"\xE0", b"\x0E\x00\x01MMD", b"VM 2018"],
        );
        let mut radio = Radio::new(mock);

        assert!(matches!(
            radio.identify().await,
            Err(crate::Error::Timeout(_))
        ));
        assert!(matches!(
            radio.require_cat_ready(),
            Err(crate::Error::CatRecoveryRequired)
        ));

        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::MmdvmMode);
        assert!(
            matches!(
                radio.require_cat_ready(),
                Err(crate::Error::CatRecoveryRequired)
            ),
            "positive binary framing must not re-enable ordinary CAT"
        );
        assert_eq!(
            radio.cat_state,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn probe_silent_link_refuses_to_probe_a_poisoned_gm_stream() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM 2018");
        let mut radio = Radio::new(mock);
        radio.gm_poisoned = true;

        assert_eq!(
            radio.probe_silent_link().await,
            LinkDiagnosis::ReconnectRequired
        );

        // The expected probe must still be queued, proving the poisoned call
        // performed no transport I/O.
        radio.gm_poisoned = false;
        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::MmdvmMode);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn probe_silent_link_refuses_unresolved_mcp_markers() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM 2018");
        let mut radio = Radio::new(mock);

        radio.mcp_saved_timeout = Some(Duration::from_secs(7));
        assert_eq!(
            radio.probe_silent_link().await,
            LinkDiagnosis::ReconnectRequired
        );
        radio.mcp_saved_timeout = None;

        radio.mcp_pending_exit_error = Some(crate::Error::CommandRejected {
            mnemonic: "0M".to_string(),
        });
        assert_eq!(
            radio.probe_silent_link().await,
            LinkDiagnosis::ReconnectRequired
        );
        radio.mcp_pending_exit_error = None;

        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::MmdvmMode);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn diagnostic_reopen_reacquires_reset_transport_without_cat() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"");
        mock.expect_reopen(Ok(()));
        mock.expect(&MMDVM_GET_VERSION_PROBE, b"\xE0\x0E\x00\x01MMDVM 2018");
        let mut radio = Radio::new(mock);

        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::Unresponsive);
        radio.reopen_for_link_diagnosis().await?;
        assert!(matches!(
            radio.require_cat_ready(),
            Err(crate::Error::CatRecoveryRequired)
        ));
        assert_eq!(radio.probe_silent_link().await, LinkDiagnosis::MmdvmMode);
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[test]
    fn mmdvm_guidance_names_the_dv_gateway_menu() {
        let text = LinkDiagnosis::MmdvmMode.guidance();
        assert!(
            text.contains("650"),
            "MmdvmMode guidance should cite Menu 650: {text:?}"
        );
    }

    #[test]
    fn unresponsive_guidance_covers_the_physical_link() {
        let text = LinkDiagnosis::Unresponsive.guidance();
        assert!(
            text.contains("cable"),
            "Unresponsive guidance should mention the cable: {text:?}"
        );
    }
}
