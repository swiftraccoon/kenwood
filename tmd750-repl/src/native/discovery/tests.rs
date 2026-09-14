//! Metadata and joined-worker tests never enumerate or open a real device.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use super::*;

type TestResult = AppResult<()>;

#[test]
fn default_requires_one_exact_recognized_name() -> TestResult {
    let expected: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let other: BluetoothAddress = "01:23:45:67:89:AC".parse()?;
    for name in CANDIDATE_NAMES {
        assert_eq!(select([(&other, "TH-D75"), (&expected, name)])?, expected);
    }
    for name in [
        "TH-D75",
        "tm-d750",
        "TM-D750 ",
        "TM-D750-2",
        "",
        "stm32mp1",
        "stm32mp1-ex5241",
        "STM32MP1-EX5240",
        "stm32mp1-ex5240 ",
    ] {
        assert!(select([(&expected, name)]).is_err());
    }
    let result = select(std::iter::empty());
    assert!(matches!(result, Err(error) if error.to_string().contains("--bluetooth-address")));
    Ok(())
}

#[test]
fn ambiguous_candidates_include_each_exact_name_and_address() -> TestResult {
    let first: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let second: BluetoothAddress = "01:23:45:67:89:AC".parse()?;
    let result = select([(&first, "stm32mp1-ex5240"), (&second, "TM-D750")]);
    let error = result.err().ok_or("ambiguous candidates were selected")?;
    for expected in [
        "ambiguous",
        "stm32mp1-ex5240",
        "TM-D750",
        first.as_str(),
        second.as_str(),
        "--bluetooth-address",
    ] {
        assert!(error.to_string().contains(expected), "{error}");
    }
    Ok(())
}

#[test]
fn duplicate_and_conflicting_address_records_are_not_selected() -> TestResult {
    let first: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let second: BluetoothAddress = "01:23:45:67:89:AC".parse()?;
    for records in [
        [(&first, "TM-D750"), (&second, "TM-D750")],
        [(&first, "TM-D750"), (&first, "TM-D750")],
        [(&first, "TM-D750"), (&first, "other name")],
    ] {
        assert!(select(records).is_err());
    }
    Ok(())
}

#[tokio::test]
async fn exact_override_preserves_helper_without_inventory() -> TestResult {
    let address: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let request = Request {
        address: Some(address.clone()),
        helper: Some(PathBuf::from("/absolute/helper with spaces")),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let selected = resolve_with(&request, &AtomicBool::new(false), move |_, _| {
        let _previous = observed_calls.fetch_add(1, Ordering::Relaxed);
        Err(CommandError("explicit selection must not enumerate".to_owned()).into())
    })
    .await?;
    assert_eq!(selected.address, address);
    assert_eq!(selected.helper, request.helper);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    Ok(())
}

#[tokio::test]
async fn discovery_and_selected_endpoint_retain_the_same_helper() -> TestResult {
    for helper in [None, Some(PathBuf::from("/absolute/helper with spaces"))] {
        let expected_helper = helper.clone().map_or_else(std::env::current_exe, Ok)?;
        let worker_helper = expected_helper.clone();
        let address: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
        let worker_address = address.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let selected = resolve_with(
            &Request {
                address: None,
                helper,
            },
            &AtomicBool::new(false),
            move |observed, cancellation| {
                assert_eq!(observed, worker_helper);
                assert!(!cancellation.is_cancelled());
                let _previous = worker_calls.fetch_add(1, Ordering::Relaxed);
                Ok(vec![(worker_address, "TM-D750".to_owned())])
            },
        )
        .await?;
        assert_eq!(selected.address, address);
        assert_eq!(selected.helper, Some(expected_helper));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
    Ok(())
}

#[tokio::test]
async fn precancellation_blocks_explicit_and_automatic_selection() -> TestResult {
    let address: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    for address in [None, Some(address)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_calls = Arc::clone(&calls);
        let result = resolve_with(
            &Request {
                address,
                helper: None,
            },
            &AtomicBool::new(true),
            move |_, _| {
                let _previous = worker_calls.fetch_add(1, Ordering::Relaxed);
                Ok(Vec::new())
            },
        )
        .await;
        assert!(matches!(result, Err(error) if error.to_string().contains("cancelled")));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_reaches_active_worker_and_waits_for_its_completion() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let worker_completed = Arc::clone(&completed);
    let cancel_request = Arc::clone(&cancelled);
    let (started, ready) = tokio::sync::oneshot::channel();
    let request = Request::default();
    let address: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let selection = resolve_with(&request, &cancelled, move |_, cancellation| {
        started
            .send(())
            .map_err(|()| CommandError("cancel fixture receiver closed".to_owned()))?;
        let deadline = Instant::now() + Duration::from_secs(1);
        while !cancellation.is_cancelled() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if !cancellation.is_cancelled() {
            return Err(CommandError("helper cancellation was not forwarded".to_owned()).into());
        }
        worker_completed.store(true, Ordering::Release);
        // A native result can race with cancellation. Even successful metadata
        // must not become a newly admitted endpoint after the worker is joined.
        Ok(vec![(address, "TM-D750".to_owned())])
    });
    let interrupt = async {
        ready.await?;
        cancel_request.store(true, Ordering::Release);
        Ok::<(), tokio::sync::oneshot::error::RecvError>(())
    };
    let (result, interrupt) = tokio::join!(selection, interrupt);
    interrupt?;
    assert!(matches!(result, Err(error) if error.to_string().contains("cancelled")));
    assert!(completed.load(Ordering::Acquire));
    Ok(())
}

#[tokio::test]
async fn cancellation_at_worker_completion_rejects_late_success() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let address: BluetoothAddress = "01:23:45:67:89:AB".parse()?;
    let result = resolve_with(&Request::default(), &cancelled, move |_, _| {
        worker_cancelled.store(true, Ordering::Release);
        Ok(vec![(address, "TM-D750".to_owned())])
    })
    .await;
    assert!(matches!(result, Err(error) if error.to_string().contains("cancelled")));
    Ok(())
}

#[tokio::test]
async fn cancellation_retains_independent_inventory_failure() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let result = resolve_with(&Request::default(), &cancelled, move |_, _| {
        worker_cancelled.store(true, Ordering::Release);
        Err(io::Error::other("inventory failure fixture").into())
    })
    .await;
    let error = result.err().ok_or("cancelled selection was admitted")?;
    for expected in ["cancelled", "inventory failure fixture"] {
        assert!(error.to_string().contains(expected), "{error}");
    }
    Ok(())
}

#[tokio::test]
async fn inventory_failure_is_not_retried_or_replaced_by_name_selection() -> TestResult {
    let result = resolve_with(&Request::default(), &AtomicBool::new(false), |_, _| {
        Err(io::Error::other("inventory failure fixture").into())
    })
    .await;
    let error = result.err().ok_or("failed inventory was admitted")?;
    assert_eq!(error.to_string(), "inventory failure fixture");
    Ok(())
}
