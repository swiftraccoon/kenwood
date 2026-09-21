//! Reflector setup with a stub resolver: joining the resolver worker on
//! cancellation and expiry, the absolute setup budget, and dropping handshake
//! state that is never spawned.

use std::sync::atomic::AtomicUsize;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn cancelled_or_expired_resolution_never_dispatches_a_worker() -> TestResult {
    for cancelled in [false, true] {
        let started = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&started);
        let deadline = if cancelled {
            Deadline::now() + Duration::from_secs(1)
        } else {
            Deadline::now()
        };
        let result = resolve_joined(
            move || {
                observed.store(true, Ordering::Release);
                Ok(())
            },
            deadline,
            &AtomicBool::new(cancelled),
        )
        .await;
        assert!(result.is_err());
        assert!(!started.load(Ordering::Acquire));
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_joins_the_worker_while_the_current_thread_remains_responsive() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let returned = AtomicBool::new(false);
    let worker_finished = Arc::new(AtomicBool::new(false));
    let completion = Arc::clone(&worker_finished);
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let resolution = async {
        let result = resolve_joined(
            move || {
                started.send(()).map_err(|()| "start receiver dropped")?;
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|error| format!("fixture resolver was not released: {error}"))?;
                completion.store(true, Ordering::Release);
                Ok(())
            },
            Deadline::now() + Duration::from_secs(5),
            &cancelled,
        )
        .await;
        returned.store(true, Ordering::Release);
        result
    };
    let control = async {
        started_rx.await?;
        cancelled.store(true, Ordering::Release);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!returned.load(Ordering::Acquire));
        release.send(())?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (result, controlled) = tokio::join!(resolution, control);
    controlled?;
    assert!(worker_finished.load(Ordering::Acquire));
    assert_eq!(result, Err("reflector setup cancelled".to_owned()));
    Ok(())
}

#[tokio::test]
async fn deadline_expiry_joins_resolution_and_preserves_its_independent_failure() -> TestResult {
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let returned = AtomicBool::new(false);
    let deadline = Deadline::now() + Duration::from_millis(20);
    let resolution = async {
        let result = resolve_joined(
            move || {
                started.send(()).map_err(|()| "start receiver dropped")?;
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|error| error.to_string())?;
                Err::<(), String>("scripted DNS failure".to_owned())
            },
            deadline,
            &AtomicBool::new(false),
        )
        .await;
        returned.store(true, Ordering::Release);
        result
    };
    let control = async {
        started_rx.await?;
        tokio::time::sleep_until(deadline).await;
        assert!(!returned.load(Ordering::Acquire));
        release.send(())?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (result, controlled) = tokio::join!(resolution, control);
    controlled?;
    assert_eq!(
        result,
        Err("reflector setup deadline expired; scripted DNS failure".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn resolution_success_and_failure_are_returned_without_substitution() -> TestResult {
    let cancelled = AtomicBool::new(false);
    let deadline = Deadline::now() + Duration::from_secs(2);
    assert_eq!(
        resolve_joined(|| Ok(19), deadline, &cancelled).await,
        Ok(19)
    );
    assert_eq!(
        resolve_joined(
            || Err::<(), _>("fixture DNS failure".to_owned()),
            deadline,
            &cancelled
        )
        .await,
        Err("fixture DNS failure".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn stopped_setup_does_not_poll_the_handshake() -> TestResult {
    for cancelled in [false, true] {
        let polled = AtomicBool::new(false);
        let operation = async {
            polled.store(true, Ordering::Release);
            Ok(())
        };
        let deadline = if cancelled {
            Deadline::now() + Duration::from_secs(1)
        } else {
            Deadline::now()
        };
        let result = bounded(operation, deadline, &AtomicBool::new(cancelled)).await;
        assert!(result.is_err());
        assert!(!polled.load(Ordering::Acquire));
    }
    Ok(())
}

#[tokio::test]
async fn continuing_authentication_activity_cannot_restart_the_absolute_deadline() -> TestResult {
    let reads = AtomicUsize::new(0);
    let operation = async {
        loop {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let _previous = reads.fetch_add(1, Ordering::AcqRel);
        }
    };
    let result: Result<(), String> = bounded(
        operation,
        Deadline::now() + Duration::from_millis(30),
        &AtomicBool::new(false),
    )
    .await;
    assert_eq!(result, Err("reflector setup deadline expired".to_owned()));
    assert!(reads.load(Ordering::Acquire) > 0);
    Ok(())
}

struct PreparedOwner<'a>(&'a AtomicUsize);

impl Drop for PreparedOwner<'_> {
    fn drop(&mut self) {
        let _previous = self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[tokio::test]
async fn cancellation_racing_handshake_success_drops_unspawned_state() -> TestResult {
    let drops = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let operation = async {
        cancelled.store(true, Ordering::Release);
        Ok(PreparedOwner(&drops))
    };
    let result = bounded(
        operation,
        Deadline::now() + Duration::from_secs(1),
        &cancelled,
    )
    .await;
    assert!(matches!(result, Err(error) if error == "reflector setup cancelled"));
    assert_eq!(drops.load(Ordering::Acquire), 1);
    Ok(())
}

#[tokio::test]
async fn admitted_handshake_retains_its_exact_owner() -> TestResult {
    let drops = AtomicUsize::new(0);
    let owner = bounded(
        async { Ok(PreparedOwner(&drops)) },
        Deadline::now() + Duration::from_secs(1),
        &AtomicBool::new(false),
    )
    .await?;
    assert!(std::ptr::eq(owner.0, std::ptr::from_ref(&drops)));
    assert_eq!(drops.load(Ordering::Acquire), 0);
    drop(owner);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    Ok(())
}
