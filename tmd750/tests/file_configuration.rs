//! Public configuration-file boundaries, using independently constructed headers.

use kenwood_thd75 as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::file::{
    FILE_SIZE_FULL, FILE_SIZE_WITHOUT_STARTUP_SCREEN, HEADER_SIZE, parse_d750,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn file(full: bool) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let (length, signature) = if full {
        (FILE_SIZE_FULL, b"MCP-D750".as_slice())
    } else {
        (FILE_SIZE_WITHOUT_STARTUP_SCREEN, b"TM-D750".as_slice())
    };
    let mut bytes = vec![0xA5; length];
    bytes
        .get_mut(..signature.len())
        .ok_or("signature range missing")?
        .copy_from_slice(signature);
    bytes
        .get_mut(16..23)
        .ok_or("model marker range missing")?
        .copy_from_slice(b"TM-D750");
    *bytes.get_mut(32).ok_or("reserved byte missing")? = 0;
    Ok(bytes)
}

#[test]
fn invalid_header_markers_are_not_accepted_as_configuration_files() -> TestResult {
    for full in [false, true] {
        for offset in [0, 16, 32] {
            let mut bytes = file(full)?;
            *bytes.get_mut(offset).ok_or("header marker missing")? ^= 0x01;
            assert!(
                parse_d750(&bytes).is_err(),
                "a correctly sized file with an invalid header at {offset} must be rejected"
            );
        }
        let length = file(full)?.len();
        assert!(
            parse_d750(&vec![0; length]).is_err(),
            "file length alone cannot turn arbitrary bytes into a configuration"
        );
    }
    Ok(())
}

#[test]
fn signature_and_payload_length_must_agree() -> TestResult {
    let mut full = file(true)?;
    full.truncate(FILE_SIZE_WITHOUT_STARTUP_SCREEN);
    assert!(
        parse_d750(&full).is_err(),
        "the full-layout signature must not accept a short payload"
    );
    let mut short = file(false)?;
    short.resize(FILE_SIZE_FULL, 0xA5);
    assert!(
        parse_d750(&short).is_err(),
        "the short-layout signature must not accept an appended startup-screen payload"
    );
    Ok(())
}

#[test]
fn incomplete_headers_and_inexact_payload_lengths_are_rejected() -> TestResult {
    for length in [0, HEADER_SIZE - 1] {
        assert!(
            parse_d750(&vec![0; length]).is_err(),
            "an incomplete {length}-byte header must be rejected"
        );
    }
    for full in [false, true] {
        let mut truncated = file(full)?;
        let _last_byte = truncated.pop();
        assert!(
            parse_d750(&truncated).is_err(),
            "a truncated payload must not be accepted"
        );
        let mut extended = file(full)?;
        extended.push(0x5A);
        assert!(
            parse_d750(&extended).is_err(),
            "trailing bytes beyond the declared layout must not be ignored"
        );
    }
    Ok(())
}
