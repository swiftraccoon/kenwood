//! Hardware-free input, explicit close and log-file collision contracts.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use kenwood_transport::{MockTransport, Transport, TransportError};

use super::{
    AppResult, CleanupFailure, IgateArgument, IgatePolicyError, Radio, finish_terminal_exit,
    parse_igate_policy, read_guidance_response, reserve_log_file,
};

#[derive(Debug)]
struct RecordingTransport {
    events: Arc<Mutex<Vec<&'static str>>>,
    close_fails: bool,
}

impl RecordingTransport {
    fn record(&self, event: &'static str) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}

impl Transport for RecordingTransport {
    fn write(&mut self, _bytes: &[u8]) -> impl Future<Output = Result<(), TransportError>> {
        std::future::poll_fn(move |_| {
            self.record("write");
            std::task::Poll::Ready(Err(TransportError::Write(std::io::Error::other(
                "unexpected write",
            ))))
        })
    }

    fn read(&mut self, _buffer: &mut [u8]) -> impl Future<Output = Result<usize, TransportError>> {
        std::future::poll_fn(move |_| {
            self.record("read");
            std::task::Poll::Ready(Err(TransportError::Read(std::io::Error::other(
                "unexpected read",
            ))))
        })
    }

    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
        std::future::poll_fn(move |_| {
            self.record("close");
            std::task::Poll::Ready(if self.close_fails {
                Err(TransportError::NotFound)
            } else {
                Ok(())
            })
        })
    }

    fn reopen(&mut self) -> impl Future<Output = Result<(), TransportError>> {
        std::future::poll_fn(move |_| {
            self.record("reopen");
            std::task::Poll::Ready(Err(TransportError::NotFound))
        })
    }
}

#[derive(Debug)]
struct ScriptedTransport {
    inner: MockTransport,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Transport for ScriptedTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.writes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(bytes.to_vec());
        self.inner.write(bytes).await
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.inner.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        self.inner.reopen().await
    }
}

#[tokio::test]
async fn terminal_recovery_refuses_a_different_radio_before_cached_setting_writes() -> AppResult<()>
{
    let mut inner = MockTransport::new();
    inner.expect(b"AE\r", b"AE SYN00001,K01\r");
    inner.expect(b"AI 1\r", b"AI 1\r");
    inner.expect_reopen(Ok(()));
    inner.expect(b"ID\r", b"ID TH-D75\r");
    inner.expect(b"AE\r", b"AE SYN00002,K01\r");
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut radio = Radio::new(ScriptedTransport {
        inner,
        writes: writes.clone(),
    });
    let _initial_identity = radio.get_serial_information().await?;
    radio.set_auto_info(true).await?;
    writes
        .lock()
        .map_err(|_| "write record lock poisoned")?
        .clear();

    let error = finish_terminal_exit(radio, Ok(()))
        .await
        .err()
        .ok_or("a replacement radio was accepted")?;
    let error = error
        .downcast_ref::<kenwood_thd75::Error>()
        .ok_or("the typed radio identity error was erased")?;
    assert!(
        matches!(error, kenwood_thd75::Error::RadioIdentityMismatch { expected, actual }
        if expected.serial_number().as_str() == "SYN00001"
            && actual.serial_number().as_str() == "SYN00002"),
        "{error:?}"
    );
    assert_eq!(
        *writes.lock().map_err(|_| "write record lock poisoned")?,
        [b"ID\r".to_vec(), b"AE\r".to_vec()],
        "a replacement radio must receive only identity reads, with no cached AI replay"
    );
    Ok(())
}

#[tokio::test]
async fn closed_terminal_guidance_input_closes_without_reopen_or_radio_writes() -> AppResult<()> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let radio = Radio::new(RecordingTransport {
        events: events.clone(),
        close_fails: false,
    });
    let response = read_guidance_response(&mut &b""[..]);
    let result = finish_terminal_exit(radio, response).await;
    let error = result
        .err()
        .ok_or("closed input unexpectedly reconnected")?;
    assert!(
        matches!(error.downcast_ref::<std::io::Error>(), Some(error)
        if error.kind() == std::io::ErrorKind::UnexpectedEof),
        "{error}"
    );
    assert_eq!(
        *events.lock().map_err(|_| "event lock poisoned")?,
        ["close"],
        "closed input must not discover, reopen or write to a radio"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_input_and_connection_close_failures_are_both_retained() -> AppResult<()> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let radio = Radio::new(RecordingTransport {
        events: events.clone(),
        close_fails: true,
    });
    let result = finish_terminal_exit(radio, read_guidance_response(&mut &b""[..])).await;
    let error = result.err().ok_or("two failures unexpectedly succeeded")?;
    let combined = error
        .downcast_ref::<CleanupFailure>()
        .ok_or("cleanup error was discarded")?;
    assert!(
        combined
            .operation
            .downcast_ref::<std::io::Error>()
            .is_some(),
        "{combined:?}"
    );
    assert!(
        combined
            .cleanup
            .downcast_ref::<kenwood_thd75::Error>()
            .is_some(),
        "{combined:?}"
    );
    assert_eq!(
        *events.lock().map_err(|_| "event lock poisoned")?,
        ["close"]
    );
    Ok(())
}

#[test]
fn terminal_guidance_response_is_bounded_and_preserves_input_errors() -> AppResult<()> {
    let too_long = [b'x'; 65];
    let error = read_guidance_response(&mut &too_long[..])
        .err()
        .ok_or("oversized response accepted")?;
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    read_guidance_response(&mut &b"\n"[..])?;
    Ok(())
}

#[test]
fn session_log_collisions_never_truncate_an_existing_file() -> AppResult<()> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("kenwood-repl-log-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let (first_path, mut first) = reserve_log_file(&directory, "same-second")?;
    first.write_all(b"first session")?;
    drop(first);
    let (second_path, mut second) = reserve_log_file(&directory, "same-second")?;
    second.write_all(b"second session")?;
    drop(second);
    assert_ne!(
        first_path, second_path,
        "each invocation must reserve a distinct file"
    );
    assert_eq!(std::fs::read(&first_path)?, b"first session");
    assert_eq!(std::fs::read(&second_path)?, b"second session");
    std::fs::remove_file(first_path)?;
    std::fs::remove_file(second_path)?;
    std::fs::remove_dir(directory)?;
    Ok(())
}

#[test]
fn igate_policy_retains_named_limits_and_rejects_conversion_overflow() -> AppResult<()> {
    let policy = parse_igate_policy(&["2", "60", "30", "15"])?;
    assert_eq!(policy.receiver_locality().maximum_repeated_hops(), 2);
    assert_eq!(policy.receiver_rf_max_age().as_secs(), 3600);
    assert_eq!(policy.sender_rf_quiet_period().as_secs(), 1800);
    assert_eq!(policy.receiver_internet_quiet_period().as_secs(), 900);
    let overflow = parse_igate_policy(&["2", "60", "18446744073709551615", "15"]);
    assert!(
        matches!(
            overflow,
            Err(IgatePolicyError::Overflow(IgateArgument::SenderRfMinutes))
        ),
        "minute conversion must reject overflow: {overflow:?}"
    );
    let invalid_number = parse_igate_policy(&["2", "60", "bad", "15"]);
    assert!(
        matches!(
            invalid_number,
            Err(IgatePolicyError::Number {
                argument: IgateArgument::SenderRfMinutes,
                ..
            })
        ),
        "{invalid_number:?}"
    );
    assert!(
        matches!(
            parse_igate_policy(&["2", "61", "30", "15"]),
            Err(IgatePolicyError::Validation(_))
        ),
        "receiver recency must retain the model's bound"
    );
    Ok(())
}

#[test]
fn igate_login_rejection_retains_the_typed_server_reason() -> AppResult<()> {
    let failure = super::IgateFailure::LoginRejected {
        reason: aprs_is::LoginRejection::Unverified {
            response: "# logresp W1AW unverified".to_owned(),
        },
    };
    let source = std::error::Error::source(&failure).ok_or("login rejection source was erased")?;
    let reason = source
        .downcast_ref::<aprs_is::LoginRejection>()
        .ok_or("login rejection source changed type")?;
    assert_eq!(reason.response(), "# logresp W1AW unverified");
    Ok(())
}
