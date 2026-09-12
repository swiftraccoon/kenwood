//! Lossless classification of D-STAR destination callsigns and gateway commands.

use super::{Callsign, Module, ReflectorCallsign};

/// Parsed action from an eight-byte D-STAR URCALL field.
///
/// A received destination may request a gateway operation instead of callsign
/// routing. Classification recognizes exact wire patterns; unknown or malformed
/// fields remain available without byte replacement or normalization.
///
/// # Special URCALL patterns
///
/// - `"CQCQCQ  "`: broadcast CQ.
/// - `"       E"`: echo test.
/// - `"       U"`: unlink from the reflector.
/// - `"       I"`: request gateway information.
/// - `"REF001 A"`: link to reflector REF001, module A. A reflector name
///   occupies up to seven right-padded bytes; its module occupies byte eight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UrCallAction {
    /// Broadcast CQ: no special routing.
    Cq,
    /// Echo test: record and play back the transmission.
    Echo,
    /// Unlink: disconnect from the current reflector.
    Unlink,
    /// Request information from the gateway.
    Info,
    /// Link to a reflector and module.
    Link {
        /// Validated reflector name, such as `REF001`, `XRF012`, or `DCS003`.
        reflector: ReflectorCallsign,
        /// Module letter (A-Z).
        module: Module,
    },
    /// Route to a destination that is not a recognized special command.
    ///
    /// The callsign retains all eight receive bytes, including malformed or
    /// non-UTF-8 content. Classification never replaces, truncates, or pads it.
    Callsign(Callsign),
}

impl UrCallAction {
    /// Classify an exact-width D-STAR URCALL field.
    ///
    /// Special commands require all eight bytes to match their protocol
    /// representation. Any other field remains a lossless destination.
    #[must_use]
    pub fn classify(ur_call: Callsign) -> Self {
        Self::classify_wire_bytes(*ur_call.as_bytes())
    }

    /// Classify eight URCALL bytes without decoding them as text first.
    ///
    /// This is the raw receive-boundary form of [`Self::classify`]. Unknown
    /// and malformed values return [`Self::Callsign`] with unchanged bytes.
    #[must_use]
    pub fn classify_wire_bytes(bytes: [u8; 8]) -> Self {
        match bytes {
            exact if exact == *b"CQCQCQ  " => Self::Cq,
            exact if exact == *b"       E" => Self::Echo,
            exact if exact == *b"       U" => Self::Unlink,
            exact if exact == *b"       I" => Self::Info,
            other => Self::classify_link(other)
                .unwrap_or_else(|| Self::Callsign(Callsign::from_wire_bytes(other))),
        }
    }

    fn classify_link(bytes: [u8; 8]) -> Option<Self> {
        let [
            first,
            second,
            third,
            fourth,
            fifth,
            sixth,
            seventh,
            module_byte,
        ] = bytes;
        let reflector_bytes = [first, second, third, fourth, fifth, sixth, seventh];
        let known_prefix = matches!(reflector_bytes.get(..3)?, b"REF" | b"XRF" | b"DCS" | b"XLX");
        if !known_prefix || !is_right_padded_reflector_name(reflector_bytes) {
            return None;
        }

        let module = Module::try_from_byte(module_byte).ok()?;
        let reflector_text = std::str::from_utf8(&reflector_bytes).ok()?;
        let reflector =
            ReflectorCallsign::try_from_str(reflector_text.trim_end_matches(' ')).ok()?;
        Some(Self::Link { reflector, module })
    }
}

/// Check the reflector portion without accepting internal padding or controls.
fn is_right_padded_reflector_name(bytes: [u8; 7]) -> bool {
    let mut reached_padding = false;
    for byte in bytes {
        if byte == b' ' {
            reached_padding = true;
        } else if reached_padding || !byte.is_ascii_alphanumeric() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn urcall_classification_preserves_non_utf8_bytes() {
        let bytes = [b'W', b'1', 0xff, b'A', b'W', b' ', b' ', b' '];
        let action = UrCallAction::classify_wire_bytes(bytes);
        let UrCallAction::Callsign(callsign) = action else {
            unreachable!("opaque URCALL must remain a destination callsign");
        };
        assert_eq!(callsign.as_bytes(), &bytes);
    }

    #[test]
    fn urcall_cq() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"CQCQCQ  "),
            UrCallAction::Cq
        );
    }

    #[test]
    fn urcall_echo() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"       E"),
            UrCallAction::Echo
        );
    }

    #[test]
    fn urcall_unlink() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"       U"),
            UrCallAction::Unlink
        );
    }

    #[test]
    fn urcall_info() {
        assert_eq!(
            UrCallAction::classify_wire_bytes(*b"       I"),
            UrCallAction::Info
        );
    }

    #[test]
    fn urcall_link_ref() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"REF001 A");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid REF command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("REF001")?);
        assert_eq!(module, Module::A);
        Ok(())
    }

    #[test]
    fn urcall_link_xrf() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"XRF012 C");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid XRF command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("XRF012")?);
        assert_eq!(module, Module::C);
        Ok(())
    }

    #[test]
    fn urcall_link_dcs() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"DCS003 B");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid DCS command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("DCS003")?);
        assert_eq!(module, Module::B);
        Ok(())
    }

    #[test]
    fn urcall_link_xlx() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"XLX999 A");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("valid XLX command must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("XLX999")?);
        assert_eq!(module, Module::A);
        Ok(())
    }

    #[test]
    fn urcall_link_accepts_seven_byte_reflector_name() -> TestResult {
        let action = UrCallAction::classify_wire_bytes(*b"REF1234A");
        let UrCallAction::Link { reflector, module } = action else {
            unreachable!("seven-byte reflector name must classify as a link");
        };
        assert_eq!(reflector, ReflectorCallsign::try_from_str("REF1234")?);
        assert_eq!(module, Module::A);
        Ok(())
    }

    #[test]
    fn urcall_callsign() {
        let action = UrCallAction::classify_wire_bytes(*b"W1AW    ");
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(*b"W1AW    "))
        );
    }

    #[test]
    fn urcall_unknown_single_char() {
        let bytes = *b"       X";
        let action = UrCallAction::classify_wire_bytes(bytes);
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
        );
    }

    #[test]
    fn urcall_near_match_is_not_fabricated_into_cq() {
        let bytes = *b"CQCQCQ X";
        let action = UrCallAction::classify_wire_bytes(bytes);
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
        );
    }

    #[test]
    fn urcall_malformed_reflector_command_remains_lossless() {
        let bytes = [b'R', b'E', b'F', 0, b'0', b'1', b' ', b'A'];
        let action = UrCallAction::classify_wire_bytes(bytes);
        assert_eq!(
            action,
            UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
        );
    }

    #[test]
    fn urcall_link_rejects_internal_padding_and_lowercase_module() {
        for bytes in [*b"REF 01 A", *b"REF001 a"] {
            let action = UrCallAction::classify_wire_bytes(bytes);
            assert_eq!(
                action,
                UrCallAction::Callsign(Callsign::from_wire_bytes(bytes))
            );
        }
    }

    #[test]
    fn urcall_classifies_lossless_callsign_value() {
        let callsign = Callsign::from_wire_bytes(*b"       U");
        assert_eq!(UrCallAction::classify(callsign), UrCallAction::Unlink);
    }
}
