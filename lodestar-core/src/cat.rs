// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Minimal CAT command codec — Phase 2 exposes only the `ID` identify
//! command and a generic `ID`-response parser. Future phases wrap
//! `kenwood-thd75`'s full protocol module.

/// CAT commands that Lodestar can send over the transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatCommand {
    /// Radio identity query — `ID\r`.
    Identify,
}

/// Parsed CAT response received from the radio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatResponse {
    /// Identity response — model string after the `ID ` prefix.
    Identify {
        /// Radio model reported by the `ID` response, e.g. `TH-D75A`.
        model: String,
    },
    /// Radio returned `?` — unknown command.
    Unknown,
    /// Radio returned `N` — command not available in current mode.
    NotAvailableInMode,
    /// Raw wire bytes that didn't match any known shape.
    Raw {
        /// The trimmed UTF-8 (lossy) representation of the line.
        line: String,
    },
}

/// Encode a [`CatCommand`] to the wire bytes the radio expects.
#[must_use]
pub fn encode_cat(command: CatCommand) -> Vec<u8> {
    match command {
        CatCommand::Identify => b"ID\r".to_vec(),
    }
}

/// Parse a complete `\r`-terminated CAT response line.
///
/// `line` must be the bytes of a single response line WITHOUT the
/// trailing `\r`. Leading/trailing ASCII whitespace is trimmed;
/// non-ASCII bytes are replaced via [`String::from_utf8_lossy`].
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary — `sequence<u8>` in UDL maps to owned `Vec<u8>`; a `&[u8]` signature wouldn't cross the FFI."
)]
pub fn parse_cat_line(line: Vec<u8>) -> CatResponse {
    let text = String::from_utf8_lossy(&line).trim().to_owned();

    if text == "?" {
        return CatResponse::Unknown;
    }
    if text == "N" {
        return CatResponse::NotAvailableInMode;
    }
    if let Some(rest) = text.strip_prefix("ID ") {
        return CatResponse::Identify {
            model: rest.trim().to_owned(),
        };
    }
    CatResponse::Raw { line: text }
}

#[cfg(test)]
mod tests {
    use super::{CatCommand, CatResponse, encode_cat, parse_cat_line};

    #[test]
    fn encode_identify_is_id_cr() {
        assert_eq!(encode_cat(CatCommand::Identify), b"ID\r");
    }

    #[test]
    fn parse_identity_response() {
        let r = parse_cat_line(b"ID TH-D75A".to_vec());
        assert_eq!(
            r,
            CatResponse::Identify {
                model: "TH-D75A".to_owned(),
            },
        );
    }

    #[test]
    fn parse_unknown_response() {
        assert_eq!(parse_cat_line(b"?".to_vec()), CatResponse::Unknown);
    }

    #[test]
    fn parse_not_available_response() {
        assert_eq!(
            parse_cat_line(b"N".to_vec()),
            CatResponse::NotAvailableInMode
        );
    }

    #[test]
    fn parse_unknown_shape_is_raw() {
        assert_eq!(
            parse_cat_line(b"WEIRD STUFF".to_vec()),
            CatResponse::Raw {
                line: "WEIRD STUFF".to_owned(),
            },
        );
    }

    #[test]
    fn parse_strips_leading_trailing_whitespace() {
        let r = parse_cat_line(b"  ID TH-D75E  ".to_vec());
        assert_eq!(
            r,
            CatResponse::Identify {
                model: "TH-D75E".to_owned(),
            },
        );
    }
}
