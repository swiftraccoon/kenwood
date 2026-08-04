//! Recovery of ordinary CAT operation after an exclusive binary mode.

use crate::error::Error;
use crate::transport::Transport;

use super::{CatState, LinkState, McpPhase, Radio};

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
            CatState::BinaryProven | CatState::RecoveryRequired => {
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
    /// packet-mode exit preamble, requires a quiet line, and accepts only one
    /// isolated exact `ID TH-D75\r` response. If that cannot prove CAT framing,
    /// it closes and reopens the transport once, repeats the preamble, and
    /// performs the same isolated proof.
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

        let in_place_result = async {
            self.send_cat_recovery_preamble().await?;
            self.prove_isolated_cat_identity().await
        }
        .await;
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
            self.send_cat_recovery_preamble().await?;
            self.prove_isolated_cat_identity().await
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

    async fn prove_isolated_cat_identity(&mut self) -> Result<(), Error> {
        self.require_strict_quiet().await?;
        self.strict_expect(b"ID\r", b"ID TH-D75\r").await?;
        self.require_strict_quiet().await
    }

    fn finish_cat_recovery(&mut self) {
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
    async fn proves_cat_in_place_without_reopening() -> TestResult {
        let mut transport = MockTransport::new();
        expect_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);
        radio.desynced = true;
        radio.cat_state = CatState::RecoveryRequired;

        radio.recover_cat().await?;

        assert!(!radio.desynced);
        assert_eq!(radio.cat_state, CatState::Ready);
        assert!(!radio.cat_recovery_required());
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn reopens_once_when_the_in_place_identity_is_invalid() -> TestResult {
        let mut transport = MockTransport::new();
        expect_recovery_preamble(&mut transport);
        transport.expect(b"ID\r", b"ID OTHER\r");
        transport.expect_reopen(Ok(()));
        expect_recovery_preamble(&mut transport);
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
        expect_recovery_preamble(&mut transport);
        transport.expect_partial_then_hang(b"ID\r", b"ID TH-");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            radio.restore_cat_after_mode_exit(),
        )
        .await;
        assert!(cancelled.is_err(), "recovery unexpectedly completed");
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert!(matches!(
            radio.identify().await,
            Err(Error::CatRecoveryRequired)
        ));
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_binary_residue_cannot_satisfy_the_identity_proof() -> TestResult {
        let mut transport = MockTransport::new();
        transport.queue_read_delayed(b"binary\rID TH-D75\r", 600);
        expect_recovery_preamble(&mut transport);
        transport.expect_reopen(Ok(()));
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
