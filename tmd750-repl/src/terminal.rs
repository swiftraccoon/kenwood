//! Static guidance for CAT state checks and guarded Terminal diagnostics.

/// Explain the current Terminal interface without opening or changing a radio.
pub(super) const fn instructions() -> &'static str {
    "TM-D750 Terminal diagnostics; this command writes no setting.\n\
     dstar start refuses its unsupported Kenwood modem configuration before I/O.\n\
     Use gateway on the open CAT connection to read the persistent Gateway state.\n\
     The assigned gateway interface does not accept CAT while active.\n\
     dstar probe requires an explicit USB endpoint and --approve-live-test.\n\
     Its --manage-terminal form additionally requires an independent USB\n\
     control endpoint and a current backup; uncertain framing blocks restoration.\n\
     A GET_VERSION reply identifies framing, not SET_CONFIG compatibility."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_requires_cat_checks_and_the_independent_diagnostic_path() {
        let guidance = instructions();
        assert!(guidance.contains("this command writes no setting"));
        assert!(guidance.contains("Use gateway on the open CAT connection"));
        assert!(guidance.contains("independent USB"));
        assert!(guidance.contains("does not accept CAT while active"));
        assert!(guidance.lines().all(|line| line.chars().count() <= 80));
    }
}
