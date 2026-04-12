//! `IoOperation` discriminator for I/O failures.

/// What kind of I/O operation an [`crate::error::Error::Io`] variant
/// was attempting when it failed.
///
/// Lets consumers distinguish "couldn't connect to auth server" from
/// "lost connection to reflector" without parsing the error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IoOperation {
    /// Binding a UDP socket.
    UdpBind,
    /// Sending a UDP datagram.
    UdpSend,
    /// Receiving a UDP datagram.
    UdpRecv,
    /// Connecting the `DPlus` auth TCP stream.
    TcpAuthConnect,
    /// Writing to the `DPlus` auth TCP stream.
    TcpAuthWrite,
    /// Reading from the `DPlus` auth TCP stream.
    TcpAuthRead,
    /// HTTP GET for the host file fetcher.
    HostsFetcherHttpGet,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_operation_is_copy() {
        let a = IoOperation::UdpRecv;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn io_operation_pattern_matches() {
        let op = IoOperation::TcpAuthConnect;
        assert!(
            matches!(op, IoOperation::TcpAuthConnect),
            "expected TcpAuthConnect, got {op:?}"
        );
    }
}
