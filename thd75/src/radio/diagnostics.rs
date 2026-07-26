//! Connection diagnostics: explaining *why* a link is not carrying CAT.
//!
//! When a freshly-opened transport accepts bytes but the radio never
//! answers a CAT command, the link is physically fine yet logically
//! unusable. The TH-D75 does this whenever it is in a DV Gateway mode
//! (Reflector Terminal or Access Point): the firmware swaps its CAT
//! command parser for the MMDVM data protocol, so `ID\r` and every
//! other CAT command is silently ignored.
//!
//! [`Radio::diagnose_link`] tells the cases apart by probing with an
//! MMDVM frame rather than guessing from a CAT timeout. A reply to an
//! MMDVM `GET_VERSION` is positive proof of MMDVM mode, whereas a CAT
//! timeout alone cannot distinguish "wrong mode" from "dead cable".

use std::time::Duration;

use crate::transport::Transport;

use super::Radio;

/// MMDVM `GET_VERSION` request: sync byte `0xE0`, length `0x03`, type `0x00`.
///
/// A radio in a DV Gateway mode answers this with an `0xE0`-framed
/// version reply; a radio in any other state ignores it.
const MMDVM_GET_VERSION: [u8; 3] = [0xE0, 0x03, 0x00];

/// How long to wait for an MMDVM reply before concluding the link is
/// unresponsive. MMDVM answers in roughly 20 ms; this is generous.
const MMDVM_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a TH-D75 link is not carrying CAT control traffic.
///
/// Produced by [`Radio::diagnose_link`], which is meant to be called
/// only after a CAT command has already failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDiagnosis {
    /// A strict GM memory-read exchange was interrupted or failed with bytes
    /// potentially still in flight. No diagnostic probe was sent because only
    /// a transport reconnect can establish a clean stream.
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
An incomplete GM memory-read exchange left the command stream untrusted.\n\
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
    /// This never returns an error. A poisoned GM stream yields
    /// [`LinkDiagnosis::ReconnectRequired`] without sending a probe. A probe
    /// that cannot be written, or that draws no reply, yields
    /// [`LinkDiagnosis::Unresponsive`].
    pub async fn diagnose_link(&mut self) -> LinkDiagnosis {
        if self.require_unpoisoned_gm_stream().is_err() {
            tracing::warn!("refusing link probe on a poisoned GM stream");
            return LinkDiagnosis::ReconnectRequired;
        }
        tracing::info!("probing silent link for MMDVM mode");
        if self.transport.write(&MMDVM_GET_VERSION).await.is_err() {
            return LinkDiagnosis::Unresponsive;
        }
        let mut buf = [0u8; 64];
        let diagnosis =
            match tokio::time::timeout(MMDVM_PROBE_TIMEOUT, self.transport.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 && buf.first().is_some_and(|&b| b == 0xE0) => {
                    LinkDiagnosis::MmdvmMode
                }
                _ => LinkDiagnosis::Unresponsive,
            };
        tracing::info!(?diagnosis, "link diagnosis complete");
        diagnosis
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn diagnose_link_detects_mmdvm_mode() -> TestResult {
        let mut mock = MockTransport::new();
        // The MMDVM GET_VERSION probe draws an 0xE0-framed version reply.
        mock.expect(b"\xE0\x03\x00", b"\xE0\x0F\x00\x01MMDVM");
        let mut radio = Radio::connect(mock).await?;
        assert_eq!(radio.diagnose_link().await, LinkDiagnosis::MmdvmMode);
        Ok(())
    }

    #[tokio::test]
    async fn diagnose_link_reports_unresponsive_on_non_mmdvm_reply() -> TestResult {
        let mut mock = MockTransport::new();
        // A reply that is not 0xE0-framed is not an MMDVM modem.
        mock.expect(b"\xE0\x03\x00", b"?\r");
        let mut radio = Radio::connect(mock).await?;
        assert_eq!(radio.diagnose_link().await, LinkDiagnosis::Unresponsive);
        Ok(())
    }

    #[tokio::test]
    async fn diagnose_link_reports_unresponsive_on_empty_reply() -> TestResult {
        let mut mock = MockTransport::new();
        // No bytes back at all, so nothing recognisable answered.
        mock.expect(b"\xE0\x03\x00", b"");
        let mut radio = Radio::connect(mock).await?;
        assert_eq!(radio.diagnose_link().await, LinkDiagnosis::Unresponsive);
        Ok(())
    }

    #[tokio::test]
    async fn diagnose_link_refuses_to_probe_a_poisoned_gm_stream() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(&MMDVM_GET_VERSION, b"\xE0\x0F\x00\x01MMDVM");
        let mut radio = Radio::connect(mock).await?;
        radio.gm_poisoned = true;

        assert_eq!(
            radio.diagnose_link().await,
            LinkDiagnosis::ReconnectRequired
        );

        // The expected probe must still be queued, proving the poisoned call
        // performed no transport I/O.
        radio.gm_poisoned = false;
        assert_eq!(radio.diagnose_link().await, LinkDiagnosis::MmdvmMode);
        radio.transport.assert_complete();
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
