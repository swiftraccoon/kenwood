//! Argument rejection through the actual CAT command handlers.

use std::sync::{Arc, Mutex};

use kenwood_transport::TransportError;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct RecordingTransport {
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Transport for RecordingTransport {
    fn write(&mut self, bytes: &[u8]) -> impl Future<Output = Result<(), TransportError>> {
        std::future::poll_fn(move |_| {
            match self.writes.lock() {
                Ok(mut writes) => writes.push(bytes.to_vec()),
                Err(_) => {
                    return std::task::Poll::Ready(Err(TransportError::Write(
                        std::io::Error::other("write record lock poisoned"),
                    )));
                }
            }
            std::task::Poll::Ready(Err(TransportError::Write(std::io::Error::other(
                "unexpected CAT write",
            ))))
        })
    }

    fn read(&mut self, _buffer: &mut [u8]) -> impl Future<Output = Result<usize, TransportError>> {
        std::future::poll_fn(|_| {
            std::task::Poll::Ready(Err(TransportError::Read(std::io::Error::other(
                "unexpected CAT read",
            ))))
        })
    }

    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
        std::future::ready(Ok(()))
    }
}

#[tokio::test]
async fn invalid_supplied_bands_never_fall_back_to_a_radio_exchange() -> TestResult {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut radio = Radio::new(RecordingTransport {
        writes: writes.clone(),
    });
    frequency(&mut radio, &[]).await;
    assert_eq!(
        writes
            .lock()
            .map_err(|_| "write record lock poisoned")?
            .len(),
        1,
        "omitted band must reach the default-band read"
    );
    writes
        .lock()
        .map_err(|_| "write record lock poisoned")?
        .clear();
    let mut radio = Radio::new(RecordingTransport {
        writes: writes.clone(),
    });

    frequency(&mut radio, &["invalid"]).await;
    set_operating_mode(&mut radio, &["invalid", "fm"]).await;
    squelch(&mut radio, &["invalid", "3"]).await;
    set_power(&mut radio, &["invalid", "high"]).await;
    attenuator(&mut radio, &["invalid", "on"]).await;
    smeter(&mut radio, &["invalid"]).await;
    vfo(&mut radio, &["invalid"]).await;
    step_size(&mut radio, &["invalid", "5"]).await;
    step_up(&mut radio, &["invalid"]).await;
    step_down(&mut radio, &["invalid"]).await;
    tune(&mut radio, &["invalid", "146.520"]).await;
    recall(&mut radio, &["invalid", "5"]).await;

    assert!(
        writes
            .lock()
            .map_err(|_| "write record lock poisoned")?
            .is_empty(),
        "an invalid supplied band must not cause any CAT write"
    );
    Ok(())
}

#[tokio::test]
async fn malformed_setter_values_never_turn_into_reads() -> TestResult {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut radio = Radio::new(RecordingTransport {
        writes: writes.clone(),
    });
    bluetooth(&mut radio, &["invalid"]).await;
    dual_band(&mut radio, &["invalid"]).await;
    fm_radio(&mut radio, &["invalid"]).await;
    attenuator(&mut radio, &["a", "invalid"]).await;
    squelch(&mut radio, &["a", "invalid"]).await;
    vox(&mut radio, &["gain", "invalid"]).await;
    vox(&mut radio, &["delay", "invalid"]).await;
    step_size(&mut radio, &["a", "invalid"]).await;
    assert!(
        writes
            .lock()
            .map_err(|_| "write record lock poisoned")?
            .is_empty(),
        "malformed setter arguments must not cause a CAT read or write"
    );
    Ok(())
}
