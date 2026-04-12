//! `Failed<S, E>` — typestate-friendly error wrapper.

/// A failed state transition.
///
/// `session` is the session in its **original** state (the
/// transition failed, so the state did not change). `error` is the
/// reason the transition failed. Used by fallible transitions so
/// callers can retry without rebuilding from scratch.
#[derive(Debug)]
pub struct Failed<S, E> {
    /// The session, still in its pre-transition state.
    pub session: S,
    /// Why the transition failed.
    pub error: E,
}

impl<S: std::fmt::Debug, E: std::error::Error + 'static> std::fmt::Display for Failed<S, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session transition failed: {}", self.error)
    }
}

impl<S: std::fmt::Debug, E: std::error::Error + 'static> std::error::Error for Failed<S, E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct DummySession;

    #[test]
    fn failed_carries_session_and_error() {
        let f = Failed {
            session: DummySession,
            error: std::io::Error::new(std::io::ErrorKind::TimedOut, "test"),
        };
        let _: &dyn std::error::Error = &f;
        assert!(f.to_string().contains("test"));
    }
}
