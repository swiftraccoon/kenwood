//! Exercise DPRS sentence parsing with arbitrary UTF-8 input.
#![no_main]

use dstar_gateway_core::{ChecksumPolicy, DprsReport};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data)
        && let Ok(report) = DprsReport::parse(s, ChecksumPolicy::Ignore)
    {
        let encoded = report.to_sentence();
        let Ok(reparsed) = DprsReport::parse(&encoded, ChecksumPolicy::Verify) else {
            unreachable!("validated reports encode to checksum-valid sentences");
        };
        assert_eq!(
            reparsed.source(),
            report.source(),
            "source survives encoding"
        );
        assert_eq!(
            reparsed.symbol(),
            report.symbol(),
            "symbol survives encoding"
        );
        assert_eq!(
            reparsed.comment(),
            report.comment(),
            "comment survives encoding"
        );
        // Formatting rounds to hundredths of a minute.
        assert!(
            (f64::from(reparsed.latitude()) - f64::from(report.latitude())).abs() <= 0.000_084,
            "latitude error stays within half a hundredth of a minute"
        );
        assert!(
            (f64::from(reparsed.longitude()) - f64::from(report.longitude())).abs() <= 0.000_084,
            "longitude error stays within half a hundredth of a minute"
        );
    }
});
