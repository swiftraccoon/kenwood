//! Read-only CAT observations with explicit ownership, identity, and cleanup.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_tmd750::{Band, DvGatewayMode, Identity, Radio};
use kenwood_transport::Transport;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::capture::{CaptureTransport, Failure, TranscriptSummary};
use crate::{AppResult, CommandError};

pub(crate) const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(1_500);

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    Identity,
    Gateway,
    Status,
}

/// Keep the actual typed tuple while publishing readable evidence.
#[derive(Debug)]
pub(crate) struct IdentityEvidence(pub(crate) Identity);

impl Serialize for IdentityEvidence {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = serializer.serialize_struct("IdentityEvidence", 3)?;
        fields.serialize_field("model", &self.0.model.to_string())?;
        fields.serialize_field("firmware", self.0.firmware.as_str())?;
        fields.serialize_field("radio_type", self.0.radio_type.as_str())?;
        fields.end()
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Observation {
    pub(crate) scope: Scope,
    pub(crate) identity: Option<IdentityEvidence>,
    pub(crate) gateway: Option<u8>,
    pub(crate) band_a: Option<String>,
    pub(crate) band_b: Option<String>,
    pub(crate) operation_error: Option<Failure>,
    pub(crate) close_error: Option<Failure>,
    pub(crate) capture_error: Option<Failure>,
    pub(crate) cancelled: bool,
    pub(crate) transcript: TranscriptSummary,
}

impl Observation {
    pub(crate) const fn succeeded(&self) -> bool {
        !self.cancelled
            && self.identity.is_some()
            && self.operation_error.is_none()
            && self.close_error.is_none()
            && self.capture_error.is_none()
            && self.transcript.complete
            && match self.scope {
                Scope::Identity => true,
                Scope::Gateway => self.gateway.is_some(),
                Scope::Status => {
                    self.gateway.is_some() && self.band_a.is_some() && self.band_b.is_some()
                }
            }
    }
}

pub(crate) struct Request<'a> {
    pub(crate) scope: Scope,
    pub(crate) expected_identity: Option<&'a Identity>,
    pub(crate) expected_gateway: Option<DvGatewayMode>,
}

fn check_cancellation(cancelled: &AtomicBool) -> AppResult<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(CommandError("CAT observation cancelled at an exchange boundary".to_owned()).into())
    } else {
        Ok(())
    }
}

async fn query(
    radio: &mut Radio<impl Transport>,
    request: &Request<'_>,
    cancelled: &AtomicBool,
    result: &mut Observation,
) -> AppResult<()> {
    check_cancellation(cancelled)?;
    let identity = radio.identify().await?;
    let matched = request
        .expected_identity
        .is_none_or(|expected| *expected == identity);
    result.identity = Some(IdentityEvidence(identity));
    if !matched {
        return Err(CommandError(
            "fresh native CAT identity differs from the original tuple".to_owned(),
        )
        .into());
    }
    if matches!(request.scope, Scope::Identity) {
        return Ok(());
    }
    if matches!(request.scope, Scope::Status) {
        check_cancellation(cancelled)?;
        result.band_a = Some(radio.get_operating_mode(Band::A).await?.to_string());
        check_cancellation(cancelled)?;
        result.band_b = Some(radio.get_operating_mode(Band::B).await?.to_string());
    }
    check_cancellation(cancelled)?;
    let gateway = radio.get_dv_gateway_mode().await?;
    result.gateway = Some(gateway.into());
    if request
        .expected_gateway
        .is_some_and(|expected| expected != gateway)
    {
        return Err(CommandError(
            "fresh native CAT Gateway differs from the required state".to_owned(),
        )
        .into());
    }
    Ok(())
}

/// Consume and retire this captured owner before returning protocol evidence.
pub(crate) async fn observe(
    mut transport: CaptureTransport<impl Transport, File>,
    request: Request<'_>,
    cancelled: &AtomicBool,
) -> Observation {
    let mut result = Observation {
        scope: request.scope,
        identity: None,
        gateway: None,
        band_a: None,
        band_b: None,
        operation_error: None,
        close_error: None,
        capture_error: None,
        cancelled: false,
        transcript: transport.transcript_summary(),
    };
    let ready = transport.synchronize();
    let mut radio = Radio::new(transport);
    radio.set_timeout(EXCHANGE_TIMEOUT);
    match ready {
        Ok(()) => {
            if let Err(error) = query(&mut radio, &request, cancelled, &mut result).await {
                result.operation_error = Some(Failure::from_error(error.as_ref()));
            }
        }
        Err(error) => result.capture_error = Some(Failure::from_error(&error)),
    }
    let mut transport = radio.into_transport();
    result.close_error = super::close(&mut transport).await;
    let mut recorder = transport.into_recorder();
    if let Err(error) = recorder.synchronize() {
        result.capture_error = Some(Failure::from_error(&error));
    }
    result.transcript = recorder.summary();
    result.cancelled = cancelled.load(Ordering::Relaxed);
    result
}
