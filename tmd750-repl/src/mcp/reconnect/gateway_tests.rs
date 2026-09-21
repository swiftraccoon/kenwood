//! Tests for the identity-plus-Gateway-Off check over mock connections that
//! record every write, close and drop.

use super::*;
use std::io;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID};
use kenwood_tmd750::{FirmwareIdentity, RadioModel, RadioType};
use kenwood_transport::MockTransport;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
type OpenHook = Box<dyn FnMut() + Send>;

#[derive(Default)]
struct Observations {
    writes: Mutex<Vec<Vec<u8>>>,
    closes: AtomicUsize,
    drops: AtomicUsize,
}

struct FreshConnection {
    script: MockTransport,
    observations: Arc<Observations>,
    cancellation: Option<(Vec<u8>, Arc<AtomicBool>)>,
    close_error: Option<io::ErrorKind>,
}

impl Transport for FreshConnection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.observations
            .writes
            .lock()
            .map_err(|_| TransportError::Write(io::Error::other("write log poisoned")))?
            .push(bytes.to_vec());
        self.script.write(bytes).await?;
        if let Some((trigger, cancelled)) = &self.cancellation
            && bytes == trigger
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.script.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let _previous = self.observations.closes.fetch_add(1, Ordering::Relaxed);
        self.script.assert_complete();
        if let Some(kind) = self.close_error {
            return Err(TransportError::Disconnected(io::Error::new(
                kind,
                "fresh close failed",
            )));
        }
        self.script.close().await
    }
}

impl Drop for FreshConnection {
    fn drop(&mut self) {
        let _previous = self.observations.drops.fetch_add(1, Ordering::Relaxed);
    }
}

struct FreshBackend {
    connection: Option<FreshConnection>,
    observations: Arc<Observations>,
    snapshots: Vec<SerialCandidate>,
    on_open: Option<OpenHook>,
    opens: usize,
    enumerations: usize,
    elapsed: Duration,
}

impl FreshBackend {
    fn new(script: MockTransport) -> Self {
        let observations = Arc::new(Observations::default());
        Self {
            connection: Some(FreshConnection {
                script,
                observations: Arc::clone(&observations),
                cancellation: None,
                close_error: None,
            }),
            observations,
            snapshots: vec![endpoint()],
            on_open: None,
            opens: 0,
            enumerations: 0,
            elapsed: Duration::ZERO,
        }
    }

    fn connection(&mut self) -> Result<&mut FreshConnection, io::Error> {
        self.connection
            .as_mut()
            .ok_or_else(|| io::Error::other("fresh connection already consumed"))
    }

    fn assert_operations(&self, expected: &[&[u8]], opened: bool) -> TestResult {
        let writes = self
            .observations
            .writes
            .lock()
            .map_err(|_| "write log poisoned")?;
        let expected: Vec<Vec<u8>> = expected.iter().map(|bytes| bytes.to_vec()).collect();
        assert_eq!(*writes, expected, "exact fresh-handle command schedule");
        drop(writes);
        let count = usize::from(opened);
        assert_eq!(self.opens, count, "never retry a fresh open");
        assert_eq!(self.observations.closes.load(Ordering::Relaxed), count);
        assert_eq!(self.observations.drops.load(Ordering::Relaxed), count);
        Ok(())
    }
}

impl Backend for FreshBackend {
    type Connection = FreshConnection;

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        assert_eq!(selected.path, endpoint().path);
        assert_eq!(baud, 9600);
        self.opens += 1;
        if let Some(hook) = &mut self.on_open {
            hook();
        }
        self.connection.take().ok_or_else(|| TransportError::Open {
            path: selected.path.clone(),
            source: io::Error::other("unexpected second open"),
        })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        self.enumerations += 1;
        Ok(self.snapshots.clone())
    }

    fn now(&self) -> Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: Duration) {
        self.elapsed += duration;
    }
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.gateway-off-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn identity() -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn identity_script(firmware: &[u8], radio_type: &[u8]) -> MockTransport {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", firmware);
    script.expect(b"TY\r", radio_type);
    script
}

fn gateway_script(reply: &[u8]) -> MockTransport {
    let mut script = identity_script(b"FV 1.02\r", b"TY K,2,1\r");
    script.expect(b"GW\r", reply);
    script
}

fn recorder(file: File) -> Recorder<File> {
    Recorder::named(file, Arc::new(AtomicBool::new(false)), "fresh-cat.jsonl")
}

async fn verify_off(
    backend: &mut FreshBackend,
    file: File,
    cancelled: &AtomicBool,
) -> Result<PostExitVerification, Box<dyn std::error::Error + Send + Sync>> {
    Ok(verify_required_gateway_off(
        backend,
        &endpoint(),
        9600,
        &identity()?,
        recorder(file),
        cancelled,
    )
    .await)
}

fn assert_failed(report: &PostExitVerification, expected_stage: VerificationStage) -> TestResult {
    let VerificationOutcome::Failed { stage, .. } = &report.outcome else {
        return Err(format!("expected failure: {report:?}").into());
    };
    assert_eq!(
        serde_json::to_value(stage)?,
        serde_json::to_value(expected_stage)?
    );
    assert!(!report.succeeded());
    assert!(report.gateway_off_evidence().is_none());
    Ok(())
}

const IDENTITY_WRITES: &[&[u8]] = &[b"ID\r", b"FV\r", b"TY\r"];
const OFF_WRITES: &[&[u8]] = &[b"ID\r", b"FV\r", b"TY\r", b"GW\r"];

#[tokio::test]
async fn required_off_retains_actual_identity_and_state_after_one_closed_durable_connection()
-> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let mut backend = FreshBackend::new(gateway_script(b"GW 0\r"));
    let report = verify_off(&mut backend, file.reopen()?, &AtomicBool::new(false)).await?;
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(
        report.gateway_off_evidence(),
        Some((&identity()?, DvGatewayMode::Off))
    );
    backend.assert_operations(OFF_WRITES, true)?;
    let encoded = serde_json::to_value(&report)?;
    assert_eq!(
        encoded.get("required_gateway_mode"),
        Some(&json!({"state":"off", "raw":0}))
    );
    let attempt = encoded.get("attempt").ok_or("attempt evidence")?;
    assert_eq!(
        attempt.get("gateway_mode"),
        Some(&json!({"state":"off", "raw":0}))
    );
    assert_eq!(
        attempt.get("identity"),
        Some(&serde_json::to_value(IdentityEvidence::from(&identity()?))?)
    );
    let transcript = std::fs::read_to_string(file.path())?;
    let records: Vec<Value> = transcript
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        records
            .last()
            .and_then(|record| record.get("event"))
            .and_then(|event| event.get("kind")),
        Some(&json!("close_completed"))
    );
    Ok(())
}

#[tokio::test]
async fn required_identity_keeps_three_queries_and_the_single_attempt_serialized_shape()
-> TestResult {
    let mut backend = FreshBackend::new(identity_script(b"FV 1.02\r", b"TY K,2,1\r"));
    let recorder = recorder(tempfile::tempfile()?);
    let identity = identity()?;
    let cancelled = AtomicBool::new(false);
    let report = verify_required(
        &mut backend,
        &endpoint(),
        9600,
        &identity,
        recorder,
        &cancelled,
    )
    .await;
    assert!(report.succeeded(), "{report:?}");
    assert!(report.gateway_off_evidence().is_none());
    backend.assert_operations(IDENTITY_WRITES, true)?;
    let encoded = serde_json::to_value(&report)?;
    assert!(encoded.get("required_gateway_mode").is_none());
    assert_eq!(
        encoded.get("attempt"),
        Some(&json!({
            "endpoint": {"path":endpoint().path, "usb_vendor_id":KENWOOD_VID, "usb_product_id":TMD750_MAIN_PID},
            "open": {"status":"succeeded"},
            "identity": {"model":"TM-D750", "firmware":"1.02", "radio_type":"K,2,1"},
            "close": {"status":"succeeded"}
        }))
    );
    Ok(())
}

#[tokio::test]
async fn terminal_and_unnamed_gateway_states_are_retained_but_never_accepted() -> TestResult {
    for (reply, mode, state) in [
        (b"GW 2\r".as_slice(), DvGatewayMode::Terminal, "terminal"),
        (b"GW 1\r", DvGatewayMode::Unqualified(1), "unqualified"),
        (b"GW 255\r", DvGatewayMode::Unqualified(255), "unqualified"),
    ] {
        let mut backend = FreshBackend::new(gateway_script(reply));
        let report =
            verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
        assert_failed(&report, VerificationStage::GatewayMismatch)?;
        assert_eq!(
            report
                .attempt
                .as_ref()
                .and_then(|attempt| attempt.gateway_mode),
            Some(GatewayEvidence(mode))
        );
        let encoded = serde_json::to_value(&report)?;
        assert_eq!(
            encoded.pointer("/attempt/gateway_mode"),
            Some(&json!({"state":state, "raw":u8::from(mode)}))
        );
        backend.assert_operations(OFF_WRITES, true)?;
    }
    Ok(())
}

#[tokio::test]
async fn failed_gateway_replies_close_without_retry_or_off_evidence() -> TestResult {
    for reply in [b"GW invalid\r".as_slice(), b"N\r", b"MD 0,0\r"] {
        let mut backend = FreshBackend::new(gateway_script(reply));
        let report =
            verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
        assert_failed(&report, VerificationStage::Gateway)?;
        assert!(
            report
                .attempt
                .as_ref()
                .and_then(|attempt| attempt.gateway_mode)
                .is_none()
        );
        backend.assert_operations(OFF_WRITES, true)?;
    }
    Ok(())
}

#[tokio::test]
async fn gateway_timeout_is_bounded_and_still_releases_the_one_handle() -> TestResult {
    let mut script = identity_script(b"FV 1.02\r", b"TY K,2,1\r");
    script.expect_hang(b"GW\r");
    let mut backend = FreshBackend::new(script);
    let report = verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
    assert_failed(&report, VerificationStage::Gateway)?;
    backend.assert_operations(OFF_WRITES, true)?;
    Ok(())
}

#[tokio::test]
async fn identity_mismatch_or_failure_never_sends_the_gateway_query() -> TestResult {
    for (firmware, radio_type) in [
        (b"FV 1.03\r".as_slice(), b"TY K,2,1\r".as_slice()),
        (b"FV 1.02\r", b"TY J,2,1\r"),
    ] {
        let mut backend = FreshBackend::new(identity_script(firmware, radio_type));
        let report =
            verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
        assert_failed(&report, VerificationStage::IdentityMismatch)?;
        backend.assert_operations(IDENTITY_WRITES, true)?;
    }
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID OTHER\r");
    let mut backend = FreshBackend::new(script);
    let report = verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
    assert_failed(&report, VerificationStage::Identity)?;
    backend.assert_operations(&[b"ID\r"], true)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_before_or_during_cat_never_publishes_finalization_evidence() -> TestResult {
    let mut unopened = FreshBackend::new(MockTransport::new());
    let report = verify_off(&mut unopened, tempfile::tempfile()?, &AtomicBool::new(true)).await?;
    assert!(matches!(
        report.outcome,
        VerificationOutcome::Skipped {
            reason: SkipReason::Cancelled
        }
    ));
    assert!(report.gateway_off_evidence().is_none());
    unopened.assert_operations(&[], false)?;
    for (trigger, query_gateway) in [(b"TY\r".as_slice(), false), (b"GW\r", true)] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let script = if query_gateway {
            gateway_script(b"GW 0\r")
        } else {
            identity_script(b"FV 1.02\r", b"TY K,2,1\r")
        };
        let mut backend = FreshBackend::new(script);
        backend.connection()?.cancellation = Some((trigger.to_vec(), Arc::clone(&cancelled)));
        let report = verify_off(&mut backend, tempfile::tempfile()?, &cancelled).await?;
        assert!(matches!(report.outcome, VerificationOutcome::Cancelled));
        assert!(report.gateway_off_evidence().is_none());
        backend.assert_operations(
            if query_gateway {
                OFF_WRITES
            } else {
                IDENTITY_WRITES
            },
            true,
        )?;
        assert_eq!(
            report
                .attempt
                .as_ref()
                .and_then(|attempt| attempt.gateway_mode)
                .is_some(),
            query_gateway
        );
    }
    Ok(())
}

#[tokio::test]
async fn gateway_success_requires_close_and_a_later_close_error_preserves_gateway_failure()
-> TestResult {
    for (reply, expected) in [
        (b"GW 0\r".as_slice(), VerificationStage::Close),
        (b"GW 2\r", VerificationStage::GatewayMismatch),
    ] {
        let mut backend = FreshBackend::new(gateway_script(reply));
        backend.connection()?.close_error = Some(io::ErrorKind::BrokenPipe);
        let report =
            verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
        assert_failed(&report, expected)?;
        assert!(matches!(
            report
                .attempt
                .as_ref()
                .and_then(|attempt| attempt.close.as_ref()),
            Some(OperationOutcome::Failed { .. })
        ));
        backend.assert_operations(OFF_WRITES, true)?;
    }
    Ok(())
}

#[tokio::test]
async fn required_off_capture_failure_before_open_prevents_all_radio_traffic() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let mut backend = FreshBackend::new(MockTransport::new());
    let report = verify_off(
        &mut backend,
        File::open(file.path())?,
        &AtomicBool::new(false),
    )
    .await?;
    assert_failed(&report, VerificationStage::Capture)?;
    assert!(!report.transcript.complete);
    assert_eq!(backend.enumerations, 0);
    backend.assert_operations(&[], false)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn required_off_capture_failure_after_open_prevents_cat_and_releases_the_handle() -> TestResult
{
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let (writer, reader) = UnixStream::pair()?;
    let mut sink = Some(reader);
    let mut backend = FreshBackend::new(MockTransport::new());
    backend.on_open = Some(Box::new(move || drop(sink.take())));
    let report = verify_off(
        &mut backend,
        File::from(OwnedFd::from(writer)),
        &AtomicBool::new(false),
    )
    .await?;
    assert!(!report.transcript.complete);
    assert!(report.gateway_off_evidence().is_none());
    assert!(!report.succeeded());
    backend.assert_operations(&[], true)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn required_off_sync_failure_refuses_evidence_without_replacing_a_gateway_mismatch()
-> TestResult {
    for (reply, expected) in [
        (b"GW 0\r".as_slice(), VerificationStage::Capture),
        (b"GW 2\r", VerificationStage::GatewayMismatch),
    ] {
        let mut backend = FreshBackend::new(gateway_script(reply));
        let sink = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
        let report = verify_off(&mut backend, sink, &AtomicBool::new(false)).await?;
        assert_failed(&report, expected)?;
        assert!(!report.transcript.complete);
        backend.assert_operations(OFF_WRITES, true)?;
    }
    Ok(())
}

#[tokio::test]
async fn an_ambiguous_endpoint_cannot_produce_gateway_off_evidence() -> TestResult {
    let mut backend = FreshBackend::new(MockTransport::new());
    let mut other = endpoint();
    other.path = "/dev/cu.other-gateway-radio".to_owned();
    backend.snapshots.push(other);
    let report = verify_off(&mut backend, tempfile::tempfile()?, &AtomicBool::new(false)).await?;
    assert_failed(&report, VerificationStage::EndpointSelection)?;
    backend.assert_operations(&[], false)?;
    Ok(())
}
