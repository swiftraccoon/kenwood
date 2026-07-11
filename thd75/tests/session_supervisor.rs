//! Supervisor behavior: backoff-driven healing over the scripted mock
//! transport, with the event stream as the observable contract.

use std::time::Duration;

use kenwood_thd75::error::TransportError;
use kenwood_thd75::radio::{LinkState, Radio};
use kenwood_thd75::session::{LinkEvent, RadioSupervisor, ReconnectPolicy};
use kenwood_thd75::transport::MockTransport;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
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
async fn heal_retries_until_restore_and_reports_events() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_eof(b"FV\r"); // kill the link mid-command
    mock.expect_reopen(Err(TransportError::NotFound)); // attempt 1 fails
    mock.expect_reopen(Ok(())); // attempt 2 succeeds
    expect_identify(&mut mock);

    let radio = Radio::connect(mock).await?;
    let mut sup = RadioSupervisor::new(radio, ReconnectPolicy::default(), 5);
    let mut events = sup.events();

    let failed = sup.radio().get_firmware_version().await;
    assert!(failed.is_err(), "expected link failure, got {failed:?}");
    assert_eq!(*sup.radio().link_state().borrow(), LinkState::Down);

    sup.heal().await?;
    assert_eq!(*sup.radio().link_state().borrow(), LinkState::Up);

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
async fn heal_gives_up_after_attempt_budget() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_eof(b"FV\r");
    mock.expect_reopen(Err(TransportError::NotFound));
    mock.expect_reopen(Err(TransportError::NotFound));
    mock.expect_reopen(Err(TransportError::NotFound));

    let radio = Radio::connect(mock).await?;
    let mut sup = RadioSupervisor::new(radio, ReconnectPolicy::default(), 3);
    let mut events = sup.events();

    let failed = sup.radio().get_firmware_version().await;
    assert!(failed.is_err(), "expected link failure, got {failed:?}");

    let r = sup.heal().await;
    assert!(r.is_err(), "heal must surface the final error, got {r:?}");
    assert_eq!(*sup.radio().link_state().borrow(), LinkState::Down);

    let seen = drain_events(&mut events);
    assert!(
        matches!(seen.last(), Some(LinkEvent::GaveUp { attempts: 3 })),
        "last event should be GaveUp after 3 attempts: {seen:?}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn heal_is_noop_when_link_up() -> TestResult {
    let mock = MockTransport::new();
    let radio = Radio::connect(mock).await?;
    let mut sup = RadioSupervisor::new(radio, ReconnectPolicy::default(), 3);
    let mut events = sup.events();

    sup.heal().await?;
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

    let radio = Radio::connect(mock).await?;
    let policy = ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(8));
    let mut sup = RadioSupervisor::new(radio, policy, 5);
    let mut events = sup.events();

    let failed = sup.radio().get_firmware_version().await;
    assert!(failed.is_err(), "expected link failure, got {failed:?}");
    sup.heal().await?;

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
    let radio = Radio::connect(mock).await?;
    let sup = RadioSupervisor::new(radio, ReconnectPolicy::default(), 3);
    let mut radio = sup.into_inner();
    let info = radio.identify().await?;
    assert!(info.model.contains("TH-D75"), "got {}", info.model);
    Ok(())
}
