//! Link-recovery behavior: backoff-driven retries over the scripted mock
//! transport, with the event stream as the observable contract.

use std::time::Duration;

use kenwood_thd75::error::TransportError;
use kenwood_thd75::radio::{LinkState, Radio};
use kenwood_thd75::session::{
    LinkEvent, RadioLinkRecovery, ReconnectAttemptLimit, ReconnectPolicy,
};
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::RadioModel;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn expect_identify(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TH-D75\r");
}

fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<LinkEvent>) -> Vec<LinkEvent> {
    let mut seen = Vec::new();
    while let Ok(e) = rx.try_recv() {
        seen.push(e);
    }
    seen
}

#[tokio::test(start_paused = true)]
async fn recovery_retries_until_restore_and_reports_events() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_eof(b"FV\r"); // kill the link mid-command
    mock.expect_reopen(Err(TransportError::NotFound)); // attempt 1 fails
    mock.expect_reopen(Ok(())); // attempt 2 succeeds
    expect_identify(&mut mock);

    let radio = Radio::new(mock);
    let attempt_limit = ReconnectAttemptLimit::new(5)?;
    let mut recovery = RadioLinkRecovery::new(radio, ReconnectPolicy::default(), attempt_limit);
    let mut events = recovery.events();

    let failed = recovery.radio().get_firmware_version().await;
    assert!(failed.is_err(), "expected link failure, got {failed:?}");
    assert_eq!(*recovery.radio().link_state().borrow(), LinkState::Down);

    recovery.recover().await?;
    assert_eq!(*recovery.radio().link_state().borrow(), LinkState::Up);

    let seen = drain_events(&mut events);
    assert!(
        matches!(seen.first(), Some(LinkEvent::Lost)),
        "first event should be Lost: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, LinkEvent::Reconnecting { attempt: 1, .. })),
        "missing first Reconnecting: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, LinkEvent::Reconnecting { attempt: 2, .. })),
        "missing second Reconnecting: {seen:?}"
    );
    assert!(
        matches!(seen.last(), Some(LinkEvent::Restored)),
        "last event should be Restored: {seen:?}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn recovery_gives_up_after_attempt_budget() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_eof(b"FV\r");
    mock.expect_reopen(Err(TransportError::NotFound));
    mock.expect_reopen(Err(TransportError::NotFound));
    mock.expect_reopen(Err(TransportError::NotFound));

    let radio = Radio::new(mock);
    let attempt_limit = ReconnectAttemptLimit::new(3)?;
    let mut recovery = RadioLinkRecovery::new(radio, ReconnectPolicy::default(), attempt_limit);
    let mut events = recovery.events();

    let failed = recovery.radio().get_firmware_version().await;
    assert!(failed.is_err(), "expected link failure, got {failed:?}");

    let r = recovery.recover().await;
    assert!(
        r.is_err(),
        "recovery must surface the final error, got {r:?}"
    );
    assert_eq!(*recovery.radio().link_state().borrow(), LinkState::Down);

    let seen = drain_events(&mut events);
    assert!(
        matches!(seen.last(), Some(LinkEvent::GaveUp { attempts: 3 })),
        "last event should be GaveUp after 3 attempts: {seen:?}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn recovery_is_noop_when_link_up() -> TestResult {
    let mock = MockTransport::new();
    let radio = Radio::new(mock);
    let attempt_limit = ReconnectAttemptLimit::new(3)?;
    let mut recovery = RadioLinkRecovery::new(radio, ReconnectPolicy::default(), attempt_limit);
    let mut events = recovery.events();

    recovery.recover().await?;
    let seen = drain_events(&mut events);
    assert!(seen.is_empty(), "no events for a healthy link: {seen:?}");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn backoff_delays_follow_policy() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_eof(b"FV\r");
    mock.expect_reopen(Err(TransportError::NotFound));
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);

    let radio = Radio::new(mock);
    let policy = ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(8))?;
    let attempt_limit = ReconnectAttemptLimit::new(5)?;
    let mut recovery = RadioLinkRecovery::new(radio, policy, attempt_limit);
    let mut events = recovery.events();

    let failed = recovery.radio().get_firmware_version().await;
    assert!(failed.is_err(), "expected link failure, got {failed:?}");
    recovery.recover().await?;

    let delays: Vec<Duration> = drain_events(&mut events)
        .into_iter()
        .filter_map(|e| match e {
            LinkEvent::Reconnecting { next_delay, .. } => Some(next_delay),
            _ => None,
        })
        .collect();
    assert_eq!(
        delays,
        vec![Duration::from_secs(1), Duration::from_secs(2)],
        "delays must follow the exponential policy"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn into_inner_returns_the_radio() -> TestResult {
    let mut mock = MockTransport::new();
    expect_identify(&mut mock);
    let radio = Radio::new(mock);
    let attempt_limit = ReconnectAttemptLimit::new(3)?;
    let recovery = RadioLinkRecovery::new(radio, ReconnectPolicy::default(), attempt_limit);
    let mut radio = recovery.into_inner();
    let info = radio.identify().await?;
    assert_eq!(info.model, RadioModel::ThD75);
    Ok(())
}
