//! `EchoLink` memory types (Menu No. 164).
//!
//! `EchoLink` is a `VoIP` system that links amateur radio stations over the
//! internet. The TH-D75 supports 10 `EchoLink` memory slots for storing
//! frequently used node numbers and their associated station names for
//! quick access via DTMF dialing.
//!
//! Per User Manual Chapter 11:
//!
//! - `EchoLink` memory channels are separate from normal DTMF memory.
//! - They do NOT store operating frequencies, tones, or power information.
//! - Each slot stores a callsign/name (up to 8 characters) and a node
//!   number or DTMF code (up to 8 digits).
//! - The radio supports `EchoLink` "Connect by Call" (prefix `C`) and
//!   "Query by Call" (prefix `07`) functions with automatic callsign-to-DTMF
//!   conversion.
//! - When only a name is stored (no code), the "Connect Call" function
//!   automatically converts the callsign to DTMF with `C` prefix and `#` suffix.
//!
//! These types model `EchoLink` settings from the TH-D75's menu system.
//! Derived from the capability gap analysis feature 138.

// ---------------------------------------------------------------------------
// EchoLink memory slot
// ---------------------------------------------------------------------------

/// An `EchoLink` memory slot.
///
/// The TH-D75 provides 10 `EchoLink` memory slots (0-9), each storing
/// a station name and node number. Node numbers are dialed via DTMF
/// to connect to the remote `EchoLink` station through a repeater's
/// `EchoLink` interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EchoLinkMemory {
    /// Slot index (0-9).
    pub slot: EchoLinkSlot,
    /// Station name or callsign (up to 8 characters).
    pub name: EchoLinkName,
    /// `EchoLink` node number (up to 6 digits).
    pub node_number: EchoLinkNode,
}

/// `EchoLink` memory slot index (0-9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EchoLinkSlot(u8);

impl EchoLinkSlot {
    /// Maximum slot index.
    pub const MAX: u8 = 9;

    /// Total number of `EchoLink` memory slots.
    pub const COUNT: usize = 10;

    /// Creates a new `EchoLink` memory slot index.
    ///
    /// # Errors
    ///
    /// Returns `None` if the index exceeds 9.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        if index <= Self::MAX {
            Some(Self(index))
        } else {
            None
        }
    }

    /// Returns the slot index.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

/// `EchoLink` station name (up to 8 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct EchoLinkName(String);

impl EchoLinkName {
    /// Maximum length of an `EchoLink` station name.
    pub const MAX_LEN: usize = 8;

    /// Creates a new `EchoLink` station name.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 8 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
        }
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `EchoLink` node number (up to 6 digits).
///
/// `EchoLink` node numbers are numeric identifiers assigned to each
/// registered station. They are transmitted via DTMF tones through
/// a repeater to initiate an `EchoLink` connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct EchoLinkNode(String);

impl EchoLinkNode {
    /// Maximum length of an `EchoLink` node number.
    pub const MAX_LEN: usize = 6;

    /// Creates a new `EchoLink` node number.
    ///
    /// # Errors
    ///
    /// Returns `None` if the string exceeds 6 characters or contains
    /// non-digit characters.
    #[must_use]
    pub fn new(number: &str) -> Option<Self> {
        if number.len() <= Self::MAX_LEN && number.chars().all(|c| c.is_ascii_digit()) {
            Some(Self(number.to_owned()))
        } else {
            None
        }
    }

    /// Returns the node number as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` if the node number is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echolink_slot_valid_range() {
        for i in 0u8..=9 {
            assert!(EchoLinkSlot::new(i).is_some());
        }
    }

    #[test]
    fn echolink_slot_invalid() {
        assert!(EchoLinkSlot::new(10).is_none());
    }

    #[test]
    fn echolink_slot_index() {
        let slot = EchoLinkSlot::new(5).unwrap();
        assert_eq!(slot.index(), 5);
    }

    #[test]
    fn echolink_name_valid() {
        let name = EchoLinkName::new("W1AW").unwrap();
        assert_eq!(name.as_str(), "W1AW");
    }

    #[test]
    fn echolink_name_max_length() {
        let name = EchoLinkName::new("12345678").unwrap();
        assert_eq!(name.as_str().len(), 8);
    }

    #[test]
    fn echolink_name_too_long() {
        assert!(EchoLinkName::new("123456789").is_none());
    }

    #[test]
    fn echolink_node_valid() {
        let node = EchoLinkNode::new("123456").unwrap();
        assert_eq!(node.as_str(), "123456");
        assert!(!node.is_empty());
    }

    #[test]
    fn echolink_node_short() {
        let node = EchoLinkNode::new("1").unwrap();
        assert_eq!(node.as_str(), "1");
    }

    #[test]
    fn echolink_node_empty() {
        let node = EchoLinkNode::new("").unwrap();
        assert!(node.is_empty());
    }

    #[test]
    fn echolink_node_too_long() {
        assert!(EchoLinkNode::new("1234567").is_none());
    }

    #[test]
    fn echolink_node_non_digit() {
        assert!(EchoLinkNode::new("12A456").is_none());
    }

    #[test]
    fn echolink_node_special_chars() {
        assert!(EchoLinkNode::new("12*456").is_none());
    }
}
