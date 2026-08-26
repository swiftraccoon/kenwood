//! Recovery of ordinary CAT operation after an exclusive binary mode.

use crate::error::Error;
use crate::transport::Transport;

use super::{CatState, LinkState, McpPhase, Radio};

/// A radio whose CAT stream is unproven after a binary-session exit.
///
/// Binary sessions (KISS, MMDVM) leave residue on the wire when they
/// end, so the exchange boundary must be re-proved before ordinary
/// commands. This wrapper makes that obligation a compile-time fact:
/// the only paths out are [`Self::restore`] (drain residue, re-prove
/// identity) or the explicitly named [`Self::into_radio_unproven`]
/// for callers that will reconnect or run their own recovery instead.
#[must_use = "restore the CAT stream (or take the radio explicitly unproven) before further use"]
#[derive(Debug)]
pub struct DesyncedRadio<T: Transport>(Radio<T>);

impl<T: Transport> DesyncedRadio<T> {
    /// Wrap a radio handed back by a binary-session exit.
    ///
    /// Constructed by the KISS and MMDVM session exits, which are the
    /// two binary modes whose transports survive their exit.
    #[cfg(any(feature = "aprs", feature = "dstar"))]
    pub(crate) const fn new(radio: Radio<T>) -> Self {
        Self(radio)
    }

    /// Drain binary residue and re-prove the CAT exchange boundary.
    ///
    /// On success the returned radio is ready for ordinary commands.
    ///
    /// # Errors
    ///
    /// Returns the wrapper back with the recovery error; the radio's
    /// internal state still marks recovery as required, so a retry or
    /// [`Radio::reconnect`] remains possible via
    /// [`Self::into_radio_unproven`].
    pub async fn restore(mut self) -> Result<Radio<T>, (Self, Error)> {
        match self.0.restore_cat_after_mode_exit().await {
            Ok(()) => Ok(self.0),
            Err(error) => Err((self, error)),
        }
    }

    /// Take the radio without proving the CAT stream.
    ///
    /// The radio still tracks its own recovery-required state, so
    /// ordinary commands keep failing until [`Radio::recover_cat`] or
    /// [`Radio::reconnect`] succeeds; this hatch only transfers that
    /// obligation to the caller.
    #[must_use]
    pub fn into_radio_unproven(self) -> Radio<T> {
        self.0
    }
}

impl<T: Transport> Radio<T> {
    /// Recover a long-lived handle so ordinary CAT commands are safe again.
    ///
    /// This is the application-level recovery entry point. It selects the
    /// required procedure from the radio's internal ownership state:
    /// interrupted MCP cleanup, a fresh-transport proof after a strict GM
    /// failure, or the universal packet-mode exit sequence after an ambiguous
    /// CAT or binary-mode boundary. Calling it on an already-ready handle is a
    /// no-op.
    ///
    /// # Errors
    ///
    /// Returns the relevant MCP, transport, identity-proof, or state-restore
    /// error. If it returns an error, callers should inspect
    /// [`Radio::cat_recovery_required`] before deciding whether the handle can
    /// still be used.
    pub async fn recover_cat(&mut self) -> Result<(), Error> {
        if self.mcp_phase != McpPhase::Inactive || self.mcp_pending_exit_error.is_some() {
            self.recover_from_interrupted_mcp().await?;
        }

        if self.gm_poisoned {
            return self.reconnect().await;
        }

        match self.cat_state {
            CatState::Ready => Ok(()),
            CatState::BinaryProven(_) | CatState::RecoveryRequired => {
                self.restore_cat_after_mode_exit().await
            }
        }
    }

    /// Prove ordinary CAT operation after a binary-mode session returns.
    ///
    /// A [`KissSession`](super::kiss_session::KissSession) or
    /// [`MmdvmSession`](super::mmdvm_session::MmdvmSession) deliberately
    /// returns a recovery-required `Radio`: binary bytes may still be buffered
    /// after the mode-exit command succeeds. This method sends the universal
    /// read-only exact `ID TH-D75\r` fast path before any recovery write. Only
    /// if that proof fails does it send the universal packet-mode exit preamble
    /// and repeat the isolated identity proof. If the complete in-place attempt
    /// fails, it closes and reopens the transport once and applies the same
    /// proof-first policy. A normal KISS or MMDVM exit therefore retains its
    /// exact TNC data band.
    ///
    /// Call this before reporting that an APRS, KISS, MMDVM, or D-STAR session
    /// has returned to CAT mode. A successful result proves the exact TH-D75
    /// CAT identity; a failure retains both recovery errors in
    /// [`Error::CatRestorationFailed`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::CatRestorationFailed`] when both the in-place recovery
    /// and the one bounded reopened recovery attempt fail. Caller-selected
    /// auto-information and GPS streaming state is not replayed here; this
    /// operation proves only the CAT framing boundary it promises.
    pub async fn restore_cat_after_mode_exit(&mut self) -> Result<(), Error> {
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }
        self.require_unpoisoned_gm_stream()?;

        // Set before the first await. Cancellation anywhere in either attempt
        // therefore leaves ordinary CAT unavailable.
        self.cat_state = CatState::RecoveryRequired;
        self.desynced = true;
        self.codec.clear();
        let _previous = self.link_state_tx.send_replace(LinkState::Down);

        let in_place_result = self.prove_cat_or_recover_packet_mode().await;
        let in_place = match in_place_result {
            Ok(()) => {
                self.finish_cat_recovery();
                return Ok(());
            }
            Err(error) => error,
        };

        tracing::warn!(
            error = %in_place,
            "in-place CAT restoration failed after binary-mode exit; reopening once"
        );

        let reconnect_result = async {
            if let Err(error) = self.close_transport().await {
                tracing::debug!(%error, "close before mode-exit recovery reopen failed");
            }
            self.transport.reopen().await.map_err(Error::Transport)?;
            self.codec.clear();
            self.last_cmd_time = None;
            self.firmware_version = None;
            self.tuning_mode_a = None;
            self.tuning_mode_b = None;
            self.prove_cat_or_recover_packet_mode().await
        }
        .await;

        match reconnect_result {
            Ok(()) => {
                self.finish_cat_recovery();
                Ok(())
            }
            Err(reconnect) => Err(Error::CatRestorationFailed {
                in_place: Box::new(in_place),
                reconnect: Box::new(reconnect),
            }),
        }
    }

    pub(super) async fn prove_isolated_cat_identity(&mut self) -> Result<(), Error> {
        self.require_strict_quiet().await?;
        self.strict_expect(b"ID\r", b"ID TH-D75\r").await?;
        self.require_strict_quiet().await
    }

    pub(super) fn finish_cat_recovery(&mut self) {
        // No await may appear between the exact proof and these transitions.
        self.codec.clear();
        self.desynced = false;
        self.cat_state = CatState::Ready;
        let _previous = self.link_state_tx.send_replace(LinkState::Up);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ProtocolError, TransportError};
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn expect_recovery_preamble(transport: &mut MockTransport) {
        transport.expect(b"\r", b"");
        transport.expect(b"\r", b"");
        transport.expect(&[0x03], b"");
        transport.expect(&[0xC0, 0xFF, 0xC0], b"");
        transport.expect(b"\rTC 1\r", b"");
        transport.expect(b"TN 0,0\r", b"");
    }

    #[tokio::test(start_paused = true)]
    async fn read_only_fast_path_preserves_band_b_without_reopening() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"TN\r", b"TN 0,1\r");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);
        radio.desynced = true;
        radio.cat_state = CatState::RecoveryRequired;

        radio.recover_cat().await?;
        let tnc = radio.get_tnc_mode().await?;

        assert!(!radio.desynced);
        assert_eq!(radio.cat_state, CatState::Ready);
        assert!(!radio.cat_recovery_required());
        assert_eq!(tnc.data_band, crate::types::TncDataBand::B);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn reopens_once_when_the_in_place_identity_is_invalid() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID OTHER\r");
        expect_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID OTHER\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);
        radio.desynced = true;
        radio.cat_state = CatState::RecoveryRequired;

        radio.restore_cat_after_mode_exit().await?;

        assert!(!radio.desynced);
        assert_eq!(radio.cat_state, CatState::Ready);
        assert!(!radio.gm_poisoned);
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn retains_both_failures_when_reopening_cannot_restore_cat() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID OTHER\r");
        expect_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID OTHER\r");
        transport.expect_reopen(Err(TransportError::ReopenUnsupported));
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);
        radio.desynced = true;
        radio.cat_state = CatState::RecoveryRequired;

        let Err(error) = radio.restore_cat_after_mode_exit().await else {
            return Err("both failed recovery attempts unexpectedly succeeded".into());
        };

        let Error::CatRestorationFailed {
            in_place,
            reconnect,
        } = error
        else {
            return Err(format!("expected CatRestorationFailed, got {error:?}").into());
        };
        assert!(matches!(
            *in_place,
            Error::Protocol(ProtocolError::UnexpectedResponse { .. })
        ));
        assert!(matches!(
            *reconnect,
            Error::Transport(TransportError::ReopenUnsupported)
        ));
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert!(!radio.gm_poisoned);
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_keeps_ordinary_cat_blocked() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"ID\r", b"ID OTHER\r");
        expect_recovery_preamble(&mut transport);
        transport.expect_partial_then_hang(b"ID\r", b"ID TH-");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            radio.restore_cat_after_mode_exit(),
        )
        .await;
        assert!(cancelled.is_err(), "recovery unexpectedly completed");
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert!(matches!(
            radio.identify().await,
            Err(Error::CatRecoveryRequired)
        ));
        assert_eq!(
            radio.transport.writes(),
            &[
                b"ID\r".to_vec(),
                b"\r".to_vec(),
                b"\r".to_vec(),
                vec![0x03],
                vec![0xC0, 0xFF, 0xC0],
                b"\rTC 1\r".to_vec(),
                b"TN 0,0\r".to_vec(),
                b"ID\r".to_vec(),
            ],
            "cancellation must land only after the fallback identity exchange begins"
        );
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_binary_residue_cannot_satisfy_the_identity_proof() -> TestResult {
        let mut transport = MockTransport::new();
        transport.queue_read_delayed(b"binary\rID TH-D75\r", 600);
        transport.expect_hang(b"ID\r");
        expect_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID OTHER\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"N\r");
        expect_recovery_preamble(&mut transport);
        transport.expect_hang(b"ID\r");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);

        let Err(error) = radio.restore_cat_after_mode_exit().await else {
            return Err(
                "delayed residue without a real post-write ID response restored CAT".into(),
            );
        };
        let Error::CatRestorationFailed { in_place, .. } = error else {
            return Err(format!("expected CatRestorationFailed, got {error:?}").into());
        };
        assert!(matches!(
            *in_place,
            Error::Protocol(ProtocolError::UnexpectedResponse { .. })
        ));
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }
}
