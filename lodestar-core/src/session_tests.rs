//! Connection, sender and task-completion boundary regressions.

use std::time::Duration;

use dstar_gateway_core::{HeaderFlags, Suffix};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn candidate() -> ReflectorConnectionCandidate {
    ReflectorConnectionCandidate {
        callsign: "W1AW".to_owned(),
        reflector: Reflector {
            name: "XRF030".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 30001,
            protocol: ReflectorProtocol::DExtra,
            description: String::new(),
        },
        local_module: "B".to_owned(),
        reflector_module: "C".to_owned(),
    }
}

fn session(backend: Backend) -> Result<Arc<ReflectorSession>, ReflectorError> {
    let config = ReflectorConnectionConfig::new(candidate())?;
    Ok(Arc::new(ReflectorSession {
        backend: Mutex::new(Some(backend)),
        callsign: "W1AW".to_owned(),
        reflector_name: "XRF030".to_owned(),
        local_module: "B".to_owned(),
        reflector_module: "C".to_owned(),
        protocol: ReflectorProtocol::DExtra,
        station_callsign: config.station,
        local_module_typed: config.local,
        reflector_callsign: config.reflector,
        reflector_module_typed: config.remote,
    }))
}

#[test]
fn connection_configuration_rejects_every_invalid_candidate_domain() {
    for invalid in [
        ReflectorConnectionCandidate {
            callsign: "ABCDEFGH".to_owned(),
            ..candidate()
        },
        ReflectorConnectionCandidate {
            local_module: "BC".to_owned(),
            ..candidate()
        },
        ReflectorConnectionCandidate {
            reflector_module: "?".to_owned(),
            ..candidate()
        },
        ReflectorConnectionCandidate {
            reflector: Reflector {
                port: 0,
                ..candidate().reflector
            },
            ..candidate()
        },
        ReflectorConnectionCandidate {
            reflector: Reflector {
                host: "host:123".to_owned(),
                ..candidate().reflector
            },
            ..candidate()
        },
        ReflectorConnectionCandidate {
            reflector: Reflector {
                protocol: ReflectorProtocol::Dcs,
                ..candidate().reflector
            },
            ..candidate()
        },
    ] {
        assert!(
            ReflectorConnectionConfig::new(invalid).is_err(),
            "invalid connection input became executable"
        );
    }
}

#[tokio::test]
async fn ipv6_literals_and_bound_sockets_keep_the_endpoint_family() -> TestResult {
    let input = ReflectorConnectionCandidate {
        reflector: Reflector {
            host: "[::1]".to_owned(),
            ..candidate().reflector
        },
        ..candidate()
    };
    let config = ReflectorConnectionConfig::new(input)?;
    let peers = resolve_peers(&config).await?;
    let peer = *peers.first().ok_or("IPv6 peer missing")?;
    assert_eq!(
        peer,
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 30001)
    );
    let socket = bind_local_socket(peer).await?;
    assert!(
        socket.local_addr()?.is_ipv6(),
        "IPv6 peer received an IPv4 socket"
    );
    let ipv4 = bind_local_socket(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1)).await?;
    assert!(
        ipv4.local_addr()?.is_ipv4(),
        "IPv4 peer received an IPv6 socket"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn complete_setup_deadline_covers_pending_phases() -> TestResult {
    let _valid = ReflectorConnectionConfig::new(candidate())?;
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(36),
        with_setup_deadline(std::future::pending::<Result<(), ReflectorError>>()),
    )
    .await?;
    assert!(
        matches!(result, Err(ReflectorError::ConnectionTimeout)),
        "complete setup deadline was lost: {result:?}"
    );
    assert_eq!(start.elapsed(), Duration::from_secs(35));
    Ok(())
}

#[tokio::test]
async fn completed_worker_is_disconnected_even_while_its_backend_is_retained() -> TestResult {
    let (shutdown, _receiver) = oneshot::channel();
    let (tx, _commands) = tokio::sync::mpsc::channel(1);
    let join = tokio::spawn(async { Ok(()) });
    while !join.is_finished() {
        tokio::task::yield_now().await;
    }
    let handle = session(Backend {
        shutdown: Some(shutdown),
        join,
        tx,
    })?;
    assert!(
        !handle.clone().is_connected().await,
        "a completed worker remained connected"
    );
    let sent = tokio::time::timeout(
        Duration::from_millis(50),
        handle
            .clone()
            .send_eot(VoiceStreamId::new(1)?, VoiceSequence::ZERO),
    )
    .await?;
    assert!(
        matches!(sent, Err(ReflectorError::AlreadyDisconnected)),
        "stale backend accepted TX: {sent:?}"
    );
    handle.disconnect().await?;
    Ok(())
}

#[tokio::test]
async fn cancelled_disconnect_retains_the_join_handle_and_original_cleanup_error() -> TestResult {
    let (shutdown, receiver) = oneshot::channel();
    let (started_tx, mut started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let (tx, _commands) = tokio::sync::mpsc::channel(1);
    let join = tokio::spawn(async move {
        let _signal = receiver.await;
        let _notified = started_tx.send(());
        let _finish = finish_rx.await;
        Err("original close failure".to_owned())
    });
    let handle = session(Backend {
        shutdown: Some(shutdown),
        join,
        tx,
    })?;
    {
        let closing = handle.clone().disconnect();
        tokio::pin!(closing);
        tokio::select! {
            result = &mut closing => { result?; return Err("disconnect finished before the worker was released".into()); }
            started = &mut started_rx => started?,
        }
    }
    assert!(
        !handle.clone().is_connected().await,
        "shutdown still advertises a live session"
    );
    finish_tx
        .send(())
        .map_err(|()| "worker disappeared before completion")?;
    let result = handle.clone().disconnect().await;
    assert_eq!(
        result,
        Err(ReflectorError::Shell("original close failure".to_owned()))
    );
    assert_eq!(
        handle.disconnect().await,
        Err(ReflectorError::AlreadyDisconnected)
    );
    Ok(())
}

#[tokio::test]
async fn relay_rewrites_only_routing_and_preserves_raw_speaker_fields() -> TestResult {
    let (shutdown, _receiver) = oneshot::channel();
    let (tx, _commands) = tokio::sync::mpsc::channel(1);
    let handle = session(Backend {
        shutdown: Some(shutdown),
        join: tokio::spawn(async { Ok(()) }),
        tx,
    })?;
    let radio = DstarHeader {
        flags: HeaderFlags::from_bytes([0x80, 0x12, 0x34]),
        rpt1: Callsign::from_wire_bytes(*b"DIRECT  "),
        rpt2: Callsign::from_wire_bytes(*b"DIRECT  "),
        ur_call: Callsign::from_wire_bytes(*b"ORIGINAL"),
        my_call: Callsign::from_wire_bytes(*b"K1ABC   "),
        my_suffix: Suffix::from_wire_bytes(*b"D75 "),
    };
    let routed = handle.build_reflector_header(&radio);
    assert_eq!(routed.rpt1.as_bytes(), b"W1AW   B");
    assert_eq!(routed.rpt2.as_bytes(), b"XRF030 C");
    assert_eq!(routed.ur_call.as_bytes(), b"CQCQCQ  ");
    assert_eq!(
        (routed.flags, routed.my_call, routed.my_suffix),
        (radio.flags, radio.my_call, radio.my_suffix)
    );
    handle.disconnect().await?;
    Ok(())
}
