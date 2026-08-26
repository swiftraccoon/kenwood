//! CAT state retained while a binary radio protocol owns the transport.

use std::time::Duration;

use crate::error::Error;
use crate::protocol::{Codec, Response};
use crate::transport::Transport;
use crate::types::{FirmwareIdentity, GpsSettings, NmeaSentences, TuningMode};

use super::{BinaryProtocolProof, CatState, LinkState, McpPhase, McpWireBoundary, Radio};

/// Host-side CAT state that survives a temporary binary-protocol session.
///
/// KISS and MMDVM both take exclusive ownership of the transport. This value
/// retains only state that remains meaningful across that boundary. Rebuilding
/// deliberately resets command timing and interrupted-protocol state and marks
/// the CAT stream desynchronized so the required CAT-restoration probe drains
/// binary residue before ordinary commands resume.
pub(super) struct CatRestoreState {
    codec: Codec,
    notifications: tokio::sync::broadcast::Sender<Response>,
    timeout: Duration,
    firmware_version: Option<FirmwareIdentity>,
    tuning_mode_a: Option<TuningMode>,
    tuning_mode_b: Option<TuningMode>,
    link_state_tx: tokio::sync::watch::Sender<LinkState>,
    auto_info_enabled: bool,
    gps_settings: Option<GpsSettings>,
    gps_sentences: Option<NmeaSentences>,
}

impl<T: Transport> Radio<T> {
    /// Separate an operational CAT radio or a binary-proved link into its
    /// transport and retained state.
    ///
    /// Binary modes cannot safely start while a strict memory-read exchange is
    /// poisoned, an ordinary CAT exchange is unresolved, or an MCP transition
    /// remains unresolved. A complete binary diagnostic proof is accepted even
    /// though ordinary CAT stays blocked. Returning the intact radio on failure
    /// lets the caller recover without losing the selected transport.
    #[expect(
        clippy::result_large_err,
        reason = "the ownership-preserving failure must return the intact Radio so callers can recover it"
    )]
    pub(super) fn into_binary_mode_parts(
        self,
        expected_protocol: BinaryProtocolProof,
    ) -> Result<(T, CatRestoreState), (Self, Error)> {
        if self.gm_poisoned {
            return Err((self, Error::MemoryReadStreamPoisoned));
        }
        if self.mcp_phase != McpPhase::Inactive
            || self.mcp_saved_timeout.is_some()
            || self.mcp_pending_exit_error.is_some()
        {
            return Err((self, Error::McpInterrupted));
        }
        match self.cat_state {
            CatState::BinaryProven(proof) if proof == expected_protocol => {}
            CatState::BinaryProven(_) | CatState::Ready => {
                return Err((self, Error::BinaryModeNotProven));
            }
            CatState::RecoveryRequired => return Err((self, Error::CatRecoveryRequired)),
        }

        let Self {
            transport,
            codec,
            notifications,
            timeout,
            firmware_version,
            tuning_mode_a,
            tuning_mode_b,
            last_cmd_time: _,
            desynced: _,
            cat_state: _,
            gm_poisoned: _,
            mcp_phase: _,
            mcp_wire_boundary: _,
            mcp_saved_timeout: _,
            mcp_pending_exit_error: _,
            link_state_tx,
            auto_info_enabled,
            gps_settings,
            gps_sentences,
        } = self;

        Ok((
            transport,
            CatRestoreState {
                codec,
                notifications,
                timeout,
                firmware_version,
                tuning_mode_a,
                tuning_mode_b,
                link_state_tx,
                auto_info_enabled,
                gps_settings,
                gps_sentences,
            },
        ))
    }
}

impl CatRestoreState {
    fn rebuild<T: Transport>(
        self,
        transport: T,
        cat_state: CatState,
        desynced: bool,
        link_state: LinkState,
    ) -> Radio<T> {
        let mut codec = self.codec;
        // Framing owned by the previous protocol session must never leak into
        // the rebuilt controller, regardless of whether it will next speak
        // CAT or be consumed by another binary session.
        codec.clear();
        let _previous = self.link_state_tx.send_replace(link_state);

        Radio {
            transport,
            codec,
            notifications: self.notifications,
            timeout: self.timeout,
            firmware_version: self.firmware_version,
            tuning_mode_a: self.tuning_mode_a,
            tuning_mode_b: self.tuning_mode_b,
            last_cmd_time: None,
            desynced,
            cat_state,
            gm_poisoned: false,
            mcp_phase: McpPhase::Inactive,
            mcp_wire_boundary: McpWireBoundary::Quiescent,
            mcp_saved_timeout: None,
            mcp_pending_exit_error: None,
            link_state_tx: self.link_state_tx,
            auto_info_enabled: self.auto_info_enabled,
            gps_settings: self.gps_settings,
            gps_sentences: self.gps_sentences,
        }
    }

    /// Rebuild desynchronized CAT ownership after the binary protocol exits.
    ///
    /// The returned radio is not qualified for ordinary CAT use until
    /// [`Radio::restore_cat_after_mode_exit`] succeeds.
    pub(super) fn rebuild_desynchronized<T: Transport>(self, transport: T) -> Radio<T> {
        self.rebuild(transport, CatState::RecoveryRequired, true, LinkState::Down)
    }

    /// Rebuild ownership of the same, clean binary link after an MMDVM
    /// consumer shuts down without changing the radio's persistent mode.
    ///
    /// This is intentionally valid only for a clean in-place transport
    /// reclaim. A reconnect requires a fresh binary diagnosis and must not
    /// retain the old proof.
    #[cfg(any(feature = "dstar", test))]
    pub(super) fn rebuild_binary_proven<T: Transport>(self, transport: T) -> Radio<T> {
        self.rebuild(
            transport,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None }),
            false,
            LinkState::Up,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use crate::types::Band;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn rebuild_preserves_cat_state_and_resets_session_state() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.codec.feed(b"partial CAT")?;
        let notification_receiver = radio.notifications.subscribe();
        let link_state = radio.link_state();
        radio.set_timeout(Duration::from_millis(731));
        radio.firmware_version = Some(FirmwareIdentity::new("1.03.AZM")?);
        radio.tuning_mode_a = Some(TuningMode::Memory);
        radio.tuning_mode_b = Some(TuningMode::Call);
        radio.last_cmd_time = Some(tokio::time::Instant::now());
        radio.desynced = false;
        let _previous = radio.link_state_tx.send_replace(LinkState::Down);
        radio.auto_info_enabled = true;
        radio.gps_settings = Some(GpsSettings::new(true, true));
        radio.gps_sentences = Some(NmeaSentences::all());
        radio.cat_state = CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None });

        let (transport, restore) = radio
            .into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
            .map_err(|(_, error)| error)?;
        let radio = restore.rebuild_desynchronized(transport);

        assert!(
            radio.codec.is_empty(),
            "partial pre-session framing must not survive binary mode"
        );
        assert_eq!(radio.notifications.receiver_count(), 1);
        drop(notification_receiver);
        assert_eq!(radio.timeout, Duration::from_millis(731));
        assert_eq!(
            radio
                .firmware_version
                .as_ref()
                .map(FirmwareIdentity::as_str),
            Some("1.03.AZM")
        );
        assert_eq!(radio.cached_tuning_mode(Band::A), Some(TuningMode::Memory));
        assert_eq!(radio.cached_tuning_mode(Band::B), Some(TuningMode::Call));
        assert_eq!(*link_state.borrow(), LinkState::Down);
        assert!(radio.auto_info_enabled);
        assert_eq!(radio.gps_settings, Some(GpsSettings::new(true, true)));
        assert_eq!(radio.gps_sentences, Some(NmeaSentences::all()));

        assert!(radio.last_cmd_time.is_none());
        assert!(radio.desynced);
        assert_eq!(radio.cat_state, CatState::RecoveryRequired);
        assert!(!radio.gm_poisoned);
        assert_eq!(radio.mcp_phase, McpPhase::Inactive);
        assert!(radio.mcp_saved_timeout.is_none());
        assert!(radio.mcp_pending_exit_error.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn clean_binary_rebuild_preserves_binary_proof_without_cat_desync() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.codec.feed(b"discarded MMDVM residue")?;
        radio.last_cmd_time = Some(tokio::time::Instant::now());
        radio.desynced = true;
        radio.cat_state = CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None });
        let link_state = radio.link_state();
        let (transport, restore) = radio
            .into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
            .map_err(|(_, error)| error)?;

        let radio = restore.rebuild_binary_proven(transport);

        assert!(radio.codec.is_empty());
        assert!(radio.last_cmd_time.is_none());
        assert!(!radio.desynced);
        assert_eq!(
            radio.cat_state,
            CatState::BinaryProven(BinaryProtocolProof::Mmdvm { data_band: None })
        );
        assert_eq!(*link_state.borrow(), LinkState::Up);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn split_returns_an_unrecoverable_protocol_state_intact() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.gm_poisoned = true;

        let Err((radio, error)) =
            radio.into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
        else {
            return Err("poisoned CAT stream unexpectedly entered a binary session".into());
        };

        assert!(matches!(error, Error::MemoryReadStreamPoisoned));
        assert!(radio.gm_poisoned);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn split_rejects_an_unproved_cat_link_intact() -> TestResult {
        let radio = Radio::new(MockTransport::new());

        let Err((radio, error)) =
            radio.into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
        else {
            return Err("unproved CAT link unexpectedly entered a binary session".into());
        };

        assert!(matches!(error, Error::BinaryModeNotProven));
        assert_eq!(radio.cat_state, CatState::Ready);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn split_returns_an_interrupted_mcp_session_intact() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.mcp_phase = McpPhase::ExitSent;

        let Err((radio, error)) =
            radio.into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
        else {
            return Err("interrupted MCP state unexpectedly entered a binary session".into());
        };

        assert!(matches!(error, Error::McpInterrupted));
        assert_eq!(radio.mcp_phase, McpPhase::ExitSent);
        radio.transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn split_returns_unresolved_mcp_markers_intact() -> TestResult {
        let mut radio = Radio::new(MockTransport::new());
        radio.mcp_saved_timeout = Some(Duration::from_millis(731));

        let Err((mut radio, error)) =
            radio.into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
        else {
            return Err("saved MCP timeout unexpectedly entered a binary session".into());
        };
        assert!(matches!(error, Error::McpInterrupted));
        assert_eq!(radio.mcp_saved_timeout, Some(Duration::from_millis(731)));

        radio.mcp_saved_timeout = None;
        radio.mcp_pending_exit_error = Some(Error::CommandRejected {
            mnemonic: "0M".to_string(),
        });
        let Err((radio, error)) =
            radio.into_binary_mode_parts(BinaryProtocolProof::Mmdvm { data_band: None })
        else {
            return Err("pending MCP exit error unexpectedly entered a binary session".into());
        };
        assert!(matches!(error, Error::McpInterrupted));
        assert!(radio.mcp_pending_exit_error.is_some());
        radio.transport.assert_complete();
        Ok(())
    }
}
