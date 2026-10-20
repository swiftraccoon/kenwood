//! Retained AE serial/model identity checks at CAT recovery boundaries.

use crate::error::Error;
use crate::protocol::{self, Response};
use crate::types::{ModelCode, SerialInformation, SerialNumber};
use kenwood_transport::Transport;

use super::{CatState, LinkState, Radio};

impl<T: Transport> Radio<T> {
    /// Retain the first observed identity and refuse any replacement.
    pub(super) fn retain_serial_information(
        &mut self,
        information: &SerialInformation,
    ) -> Result<(), Error> {
        if let Some(expected) = &self.serial_information {
            if expected != information {
                let error = Error::RadioIdentityMismatch {
                    expected: Box::new(expected.clone()),
                    actual: Box::new(information.clone()),
                };
                self.gm_poisoned = true;
                self.desynced = true;
                self.cat_state = CatState::RecoveryRequired;
                let _previous = self.link_state_tx.send_replace(LinkState::Down);
                return Err(error);
            }
        } else {
            self.serial_information = Some(information.clone());
        }
        Ok(())
    }

    /// Compare the reopened link with an identity observed before recovery.
    /// The caller keeps CAT blocked throughout this strict exchange.
    pub(super) async fn prove_retained_serial_information(&mut self) -> Result<(), Error> {
        if self.serial_information.is_none() {
            return Ok(());
        }
        let frame = self
            .strict_cat_exchange(
                b"AE\r",
                3 + SerialNumber::WIRE_LEN + 1 + ModelCode::WIRE_LEN + 1,
            )
            .await?;
        let payload = frame.strip_suffix(b"\r").ok_or_else(|| {
            Self::strict_protocol_error("a CR-terminated AE response", frame.clone())
        })?;
        match protocol::parse(payload)? {
            Response::SerialInformation(information) => {
                self.retain_serial_information(&information)
            }
            _ => Err(Self::strict_protocol_error(
                "an AE serial/model response",
                frame,
            )),
        }
    }

    /// Refuse automatic streaming writes without a previously observed serial.
    pub(super) const fn require_restore_identity(&self) -> Result<(), Error> {
        if self.serial_information.is_none()
            && (self.auto_info_enabled
                || self.gps_settings.is_some()
                || self.gps_sentences.is_some())
        {
            Err(Error::RecoveryIdentityRequired)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radio::McpPhase;
    use crate::types::{GpsSettings, NmeaSentences};
    use kenwood_transport::{MockTransport, TransportError};
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn cache_streaming_state(radio: &mut Radio<MockTransport>) {
        radio.auto_info_enabled = true;
        radio.gps_settings = Some(GpsSettings::new().with_enabled(true).with_pc_output(true));
        radio.gps_sentences = Some(NmeaSentences::all());
    }

    fn assert_only_identity_writes(radio: &Radio<MockTransport>, expected: &[&[u8]]) {
        let writes: Vec<&[u8]> = radio.transport.writes().iter().map(Vec::as_slice).collect();
        assert_eq!(
            writes, expected,
            "identity failure must send no cached-state writes"
        );
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
    }

    #[tokio::test(start_paused = true)]
    async fn first_serial_identity_cannot_be_replaced_by_a_later_public_read() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect(b"AE\r", b"AE SYN00002,K01\r");
        let mut radio = Radio::new(transport);
        let original = radio.get_serial_information().await?;

        let result = radio.get_serial_information().await;
        assert!(
            matches!(result, Err(Error::RadioIdentityMismatch { ref expected, ref actual })
            if **expected == original && actual.serial_number().as_str() == "SYN00002"),
            "{result:?}"
        );
        assert_eq!(radio.serial_information.as_ref(), Some(&original));
        assert!(radio.cat_recovery_required());
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        assert!(radio.set_auto_info(true).await.is_err());
        assert_only_identity_writes(&radio, &[b"AE\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_identity_mismatch_never_replays_cached_streaming_state() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", b"AE SYN00002,K01\r");
        let mut radio = Radio::new(transport);
        let original = radio.get_serial_information().await?;
        cache_streaming_state(&mut radio);

        let result = radio.reconnect().await;
        assert!(
            matches!(result, Err(Error::RadioIdentityMismatch { ref expected, ref actual })
            if **expected == original && actual.serial_number().as_str() == "SYN00002"),
            "{result:?}"
        );
        assert_eq!(radio.serial_information.as_ref(), Some(&original));
        assert!(radio.cat_recovery_required());
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        assert_only_identity_writes(&radio, &[b"AE\r", b"ID\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn model_code_change_also_refuses_recovery() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", b"AE SYN00001,K02\r");
        let mut radio = Radio::new(transport);
        let _identity = radio.get_serial_information().await?;
        let result = radio.reconnect().await;
        assert!(
            matches!(result, Err(Error::RadioIdentityMismatch { ref actual, .. })
            if actual.model_code().as_str() == "K02"),
            "{result:?}"
        );
        assert!(radio.cat_recovery_required());
        assert_only_identity_writes(&radio, &[b"AE\r", b"ID\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn unpinned_cached_streaming_state_is_not_replayed() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(transport);
        cache_streaming_state(&mut radio);
        let result = radio.reconnect().await;
        assert!(
            matches!(result, Err(Error::RecoveryIdentityRequired)),
            "{result:?}"
        );
        assert!(radio.cat_recovery_required());
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        assert_only_identity_writes(&radio, &[b"ID\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn matching_serial_is_proved_before_every_cached_streaming_write() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect(b"AI 1\r", b"AI 1\r");
        transport.expect(b"GP 1,1\r", b"GP 1,1\r");
        transport.expect(b"GS 1,1,1,1,1,1\r", b"GS 1,1,1,1,1,1\r");
        let mut radio = Radio::new(transport);
        let _identity = radio.get_serial_information().await?;
        cache_streaming_state(&mut radio);
        radio.reconnect().await?;
        assert!(!radio.cat_recovery_required());
        assert_eq!(*radio.link_state().borrow(), LinkState::Up);
        radio.transport.assert_complete();
        radio.transport.assert_reopen_script_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_serial_proof_keeps_cat_blocked_without_cached_writes() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect_hang(b"AE\r");
        let mut radio = Radio::new(transport);
        let original = radio.get_serial_information().await?;
        cache_streaming_state(&mut radio);
        let cancelled = tokio::time::timeout(Duration::from_millis(100), radio.reconnect()).await;
        assert!(
            cancelled.is_err(),
            "serial proof must be cancelled while awaiting AE"
        );
        assert_eq!(radio.serial_information.as_ref(), Some(&original));
        assert!(radio.cat_recovery_required());
        assert!(radio.gm_poisoned);
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert_eq!(*radio.link_state().borrow(), LinkState::Down);
        assert!(radio.set_auto_info(true).await.is_err());
        assert_only_identity_writes(&radio, &[b"AE\r", b"ID\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_serial_proof_retains_poison_and_sends_no_cached_writes() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", b"AE SYN00001,K01\rAI 1\r");
        let mut radio = Radio::new(transport);
        let _identity = radio.get_serial_information().await?;
        cache_streaming_state(&mut radio);
        let result = radio.reconnect().await;
        assert!(matches!(result, Err(Error::Protocol(_))), "{result:?}");
        assert!(radio.cat_recovery_required());
        assert_only_identity_writes(&radio, &[b"AE\r", b"ID\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn mode_exit_identity_mismatch_does_not_send_a_recovery_preamble() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", b"AE SYN00002,K01\r");
        transport.pend_when_empty();
        let mut radio = Radio::new(transport);
        let _identity = radio.get_serial_information().await?;
        let result = radio.restore_cat_after_mode_exit().await;
        assert!(
            matches!(result, Err(Error::RadioIdentityMismatch { .. })),
            "{result:?}"
        );
        assert!(radio.cat_recovery_required());
        assert_only_identity_writes(&radio, &[b"AE\r", b"ID\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn mcp_exit_identity_mismatch_retains_exit_phase_and_sends_no_cached_writes() -> TestResult
    {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect(b"E", &[protocol::programming::ACK]);
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect(b"AE\r", b"AE SYN00002,K01\r");
        let mut radio = Radio::new(transport);
        let _identity = radio.get_serial_information().await?;
        cache_streaming_state(&mut radio);
        radio.mcp_phase = McpPhase::Active;
        let result = radio.recover_from_interrupted_mcp().await;
        assert!(
            matches!(result, Err(Error::McpCleanupNotProved { ref cleanup })
            if matches!(cleanup.as_ref(), Error::RadioIdentityMismatch { .. })),
            "{result:?}"
        );
        assert_eq!(radio.mcp_phase, McpPhase::ExitSent);
        assert!(radio.cat_recovery_required());
        assert_only_identity_writes(&radio, &[b"AE\r", b"E", b"ID\r", b"AE\r"]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_mcp_exit_serial_proof_retains_exit_phase() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect(b"AE\r", b"AE SYN00001,K01\r");
        transport.expect(b"E", &[protocol::programming::ACK]);
        transport.expect_reopen(Ok(()));
        transport.expect(b"ID\r", b"ID TH-D75\r");
        transport.expect_hang(b"AE\r");
        let mut radio = Radio::new(transport);
        let original = radio.get_serial_information().await?;
        cache_streaming_state(&mut radio);
        radio.mcp_phase = McpPhase::Active;
        let cancelled =
            tokio::time::timeout(Duration::from_secs(6), radio.recover_from_interrupted_mcp())
                .await;
        assert!(
            cancelled.is_err(),
            "exit must be cancelled during the AE proof"
        );
        assert_eq!(radio.serial_information.as_ref(), Some(&original));
        assert_eq!(radio.mcp_phase, McpPhase::ExitSent);
        assert!(radio.gm_poisoned);
        assert!(radio.cat_recovery_required());
        assert_only_identity_writes(&radio, &[b"AE\r", b"E", b"ID\r", b"AE\r"]);
        Ok(())
    }

    struct FailedCloseTransport {
        inner: MockTransport,
        reopen_count: usize,
    }

    impl Transport for FailedCloseTransport {
        async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            self.inner.write(data).await
        }

        async fn read(&mut self, data: &mut [u8]) -> Result<usize, TransportError> {
            self.inner.read(data).await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Err(TransportError::Disconnected(std::io::Error::other(
                "synthetic close failure",
            )))
        }

        async fn reopen(&mut self) -> Result<(), TransportError> {
            self.reopen_count += 1;
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn failed_close_prevents_reopen_and_preserves_original_error() -> TestResult {
        let mut radio = Radio::new(FailedCloseTransport {
            inner: MockTransport::new(),
            reopen_count: 0,
        });
        let result = radio.reconnect().await;
        assert!(
            matches!(result, Err(Error::Transport(TransportError::Disconnected(ref cause)))
            if cause.to_string() == "synthetic close failure"),
            "{result:?}"
        );
        assert_eq!(radio.transport.reopen_count, 0);
        assert!(radio.transport.inner.writes().is_empty());
        assert!(radio.cat_recovery_required());
        radio.transport.inner.assert_complete();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn mode_exit_recovery_does_not_reopen_after_failed_close() -> TestResult {
        let mut inner = MockTransport::new();
        inner.expect(b"ID\r", b"ID OTHER\r");
        inner.expect(b"\r", b"");
        inner.expect(b"\r", b"");
        inner.expect(&[0x03], b"");
        inner.expect(&[0xC0, 0xFF, 0xC0], b"");
        inner.expect(b"\rTC 1\r", b"");
        inner.expect(b"TN 0,0\r", b"");
        inner.expect(b"ID\r", b"ID OTHER\r");
        inner.pend_when_empty();
        let mut radio = Radio::new(FailedCloseTransport {
            inner,
            reopen_count: 0,
        });
        let result = radio.restore_cat_after_mode_exit().await;
        assert!(
            matches!(result, Err(Error::CatRestorationFailed { ref reconnect, .. })
            if matches!(reconnect.as_ref(), Error::Transport(TransportError::Disconnected(cause))
                if cause.to_string() == "synthetic close failure")),
            "{result:?}"
        );
        assert_eq!(radio.transport.reopen_count, 0);
        assert!(radio.cat_recovery_required());
        radio.transport.inner.assert_complete();
        Ok(())
    }
}
