// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Display formatting helpers: relative times and elapsed timers.

/// Compact relative-time for heard-list rows: "now" under 5 s,
/// `M:SS` under an hour, `Hh MMm` beyond.
pub(crate) fn relative_time(secs: u64) -> String {
    if secs < 5 {
        "now".to_owned()
    } else if secs < 3600 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// `M:SS` elapsed timer for the hero display.
pub(crate) fn elapsed_mmss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `YYYYMMDD HH:MM:SS` stamp in the given display offset (heard-list
/// rows).
pub(crate) fn fmt_datetime(ts: time::OffsetDateTime, offset: time::UtcOffset) -> String {
    ts.to_offset(offset)
        .format(time::macros::format_description!(
            "[year][month][day] [hour]:[minute]:[second]"
        ))
        .unwrap_or_default()
}

/// `HH:MM:SS` stamp in the given display offset (event-log lines).
pub(crate) fn fmt_time_hms(ts: time::OffsetDateTime, offset: time::UtcOffset) -> String {
    ts.to_offset(offset)
        .format(time::macros::format_description!(
            "[hour]:[minute]:[second]"
        ))
        .unwrap_or_default()
}

/// QRZ.com database URL for a callsign. Strips the SSID/suffix part
/// (anything from the first space or `/`) because QRZ indexes base calls.
pub(crate) fn qrz_url(callsign: &str) -> String {
    let base: String = callsign
        .trim()
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '/')
        .collect();
    format!("https://www.qrz.com/db/{}", base.to_uppercase())
}

/// Seconds elapsed since `ts` (clamped at zero for clock skew).
pub(crate) fn secs_since(ts: time::OffsetDateTime) -> u64 {
    let secs = (time::OffsetDateTime::now_utc() - ts).whole_seconds();
    u64::try_from(secs.max(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_is_minutes_seconds() {
        assert_eq!(elapsed_mmss(7), "0:07");
        assert_eq!(elapsed_mmss(65), "1:05");
    }

    #[test]
    fn fmt_datetime_is_compact_and_offset_aware() -> Result<(), Box<dyn std::error::Error>> {
        let ts = time::OffsetDateTime::from_unix_timestamp(0)?;
        assert_eq!(fmt_datetime(ts, time::UtcOffset::UTC), "19700101 00:00:00");
        let minus_five = time::UtcOffset::from_hms(-5, 0, 0)?;
        assert_eq!(
            fmt_datetime(ts, minus_five),
            "19691231 19:00:00",
            "offset shifts the displayed wall clock"
        );
        let ts = time::OffsetDateTime::from_unix_timestamp(1_782_899_696)?;
        assert_eq!(fmt_datetime(ts, time::UtcOffset::UTC), "20260701 09:54:56");
        Ok(())
    }

    #[test]
    fn qrz_url_uses_base_callsign() {
        assert_eq!(qrz_url("w1aw"), "https://www.qrz.com/db/W1AW");
        assert_eq!(qrz_url("W1AW /D75"), "https://www.qrz.com/db/W1AW");
        assert_eq!(qrz_url("KQ4NIT/P"), "https://www.qrz.com/db/KQ4NIT");
        assert_eq!(qrz_url("  G4ABC  "), "https://www.qrz.com/db/G4ABC");
    }

    #[test]
    fn fmt_time_hms_is_offset_aware() -> Result<(), Box<dyn std::error::Error>> {
        let ts = time::OffsetDateTime::from_unix_timestamp(0)?;
        let plus_two = time::UtcOffset::from_hms(2, 0, 0)?;
        assert_eq!(fmt_time_hms(ts, plus_two), "02:00:00");
        Ok(())
    }

    #[test]
    fn relative_time_brackets() {
        assert_eq!(relative_time(0), "now");
        assert_eq!(relative_time(4), "now");
        assert_eq!(relative_time(12), "0:12");
        assert_eq!(relative_time(160), "2:40");
        assert_eq!(relative_time(3599), "59:59");
        assert_eq!(relative_time(3600), "1h 00m");
        assert_eq!(relative_time(4500), "1h 15m");
    }
}
