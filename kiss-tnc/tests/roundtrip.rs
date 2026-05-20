//! Property-based KISS codec round-trips.

// Integration tests are separate compilation units that re-evaluate
// workspace deps; acknowledge the transitively-reachable lib dep this
// file does not name directly.
use thiserror as _;

use kiss_tnc::{KissCommand, KissFrame, KissPort, decode_kiss_frame, encode_kiss_frame};
use proptest::prelude::*;

/// Strategy producing any of the seven nibble-encoded KISS commands.
fn nibble_command() -> impl Strategy<Value = KissCommand> {
    prop_oneof![
        Just(KissCommand::Data),
        Just(KissCommand::TxDelay),
        Just(KissCommand::Persistence),
        Just(KissCommand::SlotTime),
        Just(KissCommand::TxTail),
        Just(KissCommand::FullDuplex),
        Just(KissCommand::SetHardware),
    ]
}

proptest! {
    /// Every valid port, every nibble command, and arbitrary payload
    /// data round-trips through encode -> decode unchanged.
    #[test]
    fn kiss_codec_roundtrip(
        port in (0u8..=15).prop_filter_map("valid port", KissPort::new),
        command in nibble_command(),
        data in prop::collection::vec(any::<u8>(), 0..256),
    ) {
        let frame = KissFrame {
            port,
            command,
            data: data.clone(),
        };
        let decoded = decode_kiss_frame(&encode_kiss_frame(&frame))?;
        prop_assert_eq!(decoded.port, port);
        prop_assert_eq!(decoded.command, command);
        prop_assert_eq!(decoded.data, data);
    }
}
