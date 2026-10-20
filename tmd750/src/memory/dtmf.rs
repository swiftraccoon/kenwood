//! The DTMF and `EchoLink` memories: ten channels each of a name and a code
//! that the radio's automatic dialer transmits as DTMF tones.
//!
//! The DTMF memory holds 16-byte codes from `0x40` and 16-byte names from
//! `0x100`; the `EchoLink` memory holds 8-byte codes from `0x200` and 8-byte
//! names from `0x250`, all inside the standard backup schedule. Every field
//! is zero-filled text, and a channel whose name and code are both empty is
//! unused. A code holds the DTMF characters `0` to `9`, `A` to `D`, `*` and
//! `#`, and a space for a pause; the library admits printable ASCII in names.
//!
//! On firmware 1.02, type `K,2,1`, a radio with no stored channels held both
//! memories zero. A DTMF channel with a pause in its code, and an `EchoLink`
//! channel whose name and code filled their fields, written over the
//! operation-panel USB, read back in place in a fresh session, changing only
//! their own bytes, and clearing them restored every byte of the standard
//! backup. How the radio dials a channel holding a name
//! and no code is not established.

use std::fmt;

use super::{MemoryImage, chain};
use crate::error::McpError;
use crate::protocol::mcp::regions;
use crate::radio::programming::PageReplacement;
use crate::types::Address;

#[cfg(test)]
#[path = "dtmf_tests.rs"]
mod tests;

/// Channels in each memory, `CH0` to `CH9`.
pub const DTMF_MEMORY_CHANNELS: usize = 10;
/// Bytes of a DTMF memory name or code.
pub const DTMF_FIELD_SIZE: usize = 16;
/// Bytes of an `EchoLink` memory name or code.
pub const ECHOLINK_FIELD_SIZE: usize = 8;

/// Image address of the first DTMF memory code.
const DTMF_CODES: u32 = 0x40;
/// Image address of the first DTMF memory name.
const DTMF_NAMES: u32 = 0x100;
/// Image address of the first `EchoLink` memory code.
const ECHOLINK_CODES: u32 = 0x200;
/// Image address of the first `EchoLink` memory name.
const ECHOLINK_NAMES: u32 = 0x250;

/// A DTMF memory channel's name and code, up to 16 bytes each.
pub type DtmfMemoryEntry = DtmfEntry<DTMF_FIELD_SIZE>;
/// An `EchoLink` memory channel's name and code, up to 8 bytes each.
pub type EchoLinkMemoryEntry = DtmfEntry<ECHOLINK_FIELD_SIZE>;

/// One of the two memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DtmfMemoryKind {
    /// The DTMF memory: 16-byte names and codes.
    Dtmf,
    /// The `EchoLink` memory: 8-byte names and codes, typically a callsign
    /// and a node number.
    EchoLink,
}

impl DtmfMemoryKind {
    /// Both memories.
    pub const ALL: [Self; 2] = [Self::Dtmf, Self::EchoLink];

    /// Bytes of each name and each code.
    #[must_use]
    pub const fn field_size(self) -> usize {
        match self {
            Self::Dtmf => DTMF_FIELD_SIZE,
            Self::EchoLink => ECHOLINK_FIELD_SIZE,
        }
    }

    /// Image address of `channel`'s code.
    #[must_use]
    pub const fn code_address(self, channel: DtmfChannel) -> u32 {
        let first = match self {
            Self::Dtmf => DTMF_CODES,
            Self::EchoLink => ECHOLINK_CODES,
        };
        first + self.stride() * channel.0 as u32
    }

    /// Image address of `channel`'s name.
    #[must_use]
    pub const fn name_address(self, channel: DtmfChannel) -> u32 {
        let first = match self {
            Self::Dtmf => DTMF_NAMES,
            Self::EchoLink => ECHOLINK_NAMES,
        };
        first + self.stride() * channel.0 as u32
    }

    /// [`Self::field_size`] as an address offset.
    const fn stride(self) -> u32 {
        match self {
            Self::Dtmf => 16,
            Self::EchoLink => 8,
        }
    }
}

impl fmt::Display for DtmfMemoryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Dtmf => "DTMF memory",
            Self::EchoLink => "EchoLink memory",
        })
    }
}

/// A channel number of either memory, `CH0` to `CH9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DtmfChannel(u8);

checked_integer_traits!(DtmfChannel, u8, index, DtmfMemoryError);

impl DtmfChannel {
    /// Every channel in order.
    pub const ALL: [Self; DTMF_MEMORY_CHANNELS] = [
        Self(0),
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
        Self(8),
        Self(9),
    ];

    /// Channel `index`.
    ///
    /// # Errors
    ///
    /// Returns [`DtmfMemoryError::ChannelOutOfRange`] above 9.
    pub const fn new(index: u8) -> Result<Self, DtmfMemoryError> {
        if index as usize >= DTMF_MEMORY_CHANNELS {
            Err(DtmfMemoryError::ChannelOutOfRange { channel: index })
        } else {
            Ok(Self(index))
        }
    }

    /// The channel number.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }
}

impl fmt::Display for DtmfChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "CH{}", self.0)
    }
}

/// A stored channel: a name and a code of up to `N` bytes each, not both
/// empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DtmfEntry<const N: usize> {
    name: [u8; N],
    code: [u8; N],
}

impl<const N: usize> DtmfEntry<N> {
    /// A channel named `name` that dials `code`.
    ///
    /// # Errors
    ///
    /// Returns [`DtmfMemoryError::InvalidName`] for a name longer than `N`
    /// bytes or holding a character outside space to `~`,
    /// [`DtmfMemoryError::InvalidCode`] for a code longer than `N` characters
    /// or holding a character other than `0` to `9`, `A` to `D`, `*`, `#`
    /// and space, and [`DtmfMemoryError::Empty`] when both are empty.
    pub fn new(name: &str, code: &str) -> Result<Self, DtmfMemoryError> {
        let name_field = field(name, is_name_byte).ok_or_else(|| DtmfMemoryError::InvalidName {
            name: name.to_owned(),
            capacity: N,
        })?;
        let code_field = field(code, is_code_byte).ok_or_else(|| DtmfMemoryError::InvalidCode {
            code: code.to_owned(),
            capacity: N,
        })?;
        if name.is_empty() && code.is_empty() {
            return Err(DtmfMemoryError::Empty);
        }
        Ok(Self {
            name: name_field,
            code: code_field,
        })
    }

    /// Decode a stored channel; both fields zero is an unused channel.
    ///
    /// # Errors
    ///
    /// Returns [`DtmfMemoryError::Malformed`] for a field byte outside its
    /// character set or a nonzero byte after the field's first zero.
    pub fn from_bytes(name: [u8; N], code: [u8; N]) -> Result<Option<Self>, DtmfMemoryError> {
        let malformed = |bytes: &[u8; N]| DtmfMemoryError::Malformed {
            bytes: bytes.to_vec(),
        };
        if text(&name, is_name_byte).is_none() {
            return Err(malformed(&name));
        }
        if text(&code, is_code_byte).is_none() {
            return Err(malformed(&code));
        }
        let unused = name.iter().chain(&code).all(|&byte| byte == 0);
        Ok((!unused).then_some(Self { name, code }))
    }

    /// The name, empty when none is stored.
    #[must_use]
    pub fn name(&self) -> &str {
        text(&self.name, is_name_byte).unwrap_or_default()
    }

    /// The code, empty when none is stored.
    #[must_use]
    pub fn code(&self) -> &str {
        text(&self.code, is_code_byte).unwrap_or_default()
    }

    /// The stored name field.
    #[must_use]
    pub const fn name_bytes(&self) -> &[u8; N] {
        &self.name
    }

    /// The stored code field.
    #[must_use]
    pub const fn code_bytes(&self) -> &[u8; N] {
        &self.code
    }
}

impl<const N: usize> fmt::Display for DtmfEntry<N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?} {:?}", self.code(), self.name())
    }
}

const fn is_name_byte(byte: u8) -> bool {
    matches!(byte, b' '..=b'~')
}

const fn is_code_byte(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'A'..=b'D' | b'*' | b'#' | b' ')
}

/// The zero-filled field holding `value`, or `None` when it is longer than
/// `N` bytes or holds a byte `admits` refuses.
fn field<const N: usize>(value: &str, admits: fn(u8) -> bool) -> Option<[u8; N]> {
    if value.len() > N || !value.bytes().all(admits) {
        return None;
    }
    let mut stored = [0; N];
    for (destination, source) in stored.iter_mut().zip(value.bytes()) {
        *destination = source;
    }
    Some(stored)
}

/// The text of a zero-filled field, or `None` when a byte before the first
/// zero is one `admits` refuses or a byte after it is nonzero.
fn text(bytes: &[u8], admits: fn(u8) -> bool) -> Option<&str> {
    let length = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    let (value, padding) = bytes.split_at(length);
    if value.iter().copied().all(admits) && padding.iter().all(|&byte| byte == 0) {
        std::str::from_utf8(value).ok()
    } else {
        None
    }
}

/// Both memories, unused channels `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DtmfMemories {
    dtmf: [Option<DtmfMemoryEntry>; DTMF_MEMORY_CHANNELS],
    echolink: [Option<EchoLinkMemoryEntry>; DTMF_MEMORY_CHANNELS],
}

impl DtmfMemories {
    /// Decode both memories from `image`.
    ///
    /// # Errors
    ///
    /// Returns [`DtmfMemoryError::Entry`] naming the first channel whose
    /// fields [`DtmfEntry::from_bytes`] refuses.
    pub fn read(image: &MemoryImage) -> Result<Self, DtmfMemoryError> {
        Ok(Self {
            dtmf: read_table(image, DtmfMemoryKind::Dtmf)?,
            echolink: read_table(image, DtmfMemoryKind::EchoLink)?,
        })
    }

    /// The DTMF memory channel `channel`, `None` when unused.
    #[must_use]
    pub fn dtmf(&self, channel: DtmfChannel) -> Option<&DtmfMemoryEntry> {
        self.dtmf
            .get(usize::from(channel.0))
            .and_then(Option::as_ref)
    }

    /// The `EchoLink` memory channel `channel`, `None` when unused.
    #[must_use]
    pub fn echolink(&self, channel: DtmfChannel) -> Option<&EchoLinkMemoryEntry> {
        self.echolink
            .get(usize::from(channel.0))
            .and_then(Option::as_ref)
    }

    /// Store `entry` in DTMF memory channel `channel`; `None` clears it.
    pub fn set_dtmf(&mut self, channel: DtmfChannel, entry: Option<DtmfMemoryEntry>) {
        if let Some(slot) = self.dtmf.get_mut(usize::from(channel.0)) {
            *slot = entry;
        }
    }

    /// Store `entry` in `EchoLink` memory channel `channel`; `None` clears it.
    pub fn set_echolink(&mut self, channel: DtmfChannel, entry: Option<EchoLinkMemoryEntry>) {
        if let Some(slot) = self.echolink.get_mut(usize::from(channel.0)) {
            *slot = entry;
        }
    }

    /// The writes of the pages whose bytes change, keeping every byte outside
    /// the memories' fields as `image` holds it: the page from `0x38` holding
    /// the DTMF codes, the page from `0x100` holding the DTMF names, and the
    /// page from `0x200` holding the `EchoLink` memory.
    ///
    /// Apply them with [`crate::radio::McpSession::compare_exchange_pages`],
    /// which compares each page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(
        &self,
        image: &MemoryImage,
    ) -> Result<Vec<PageReplacement>, crate::Error> {
        let mut fields = Vec::new();
        push_fields(&mut fields, DtmfMemoryKind::Dtmf, &self.dtmf);
        push_fields(&mut fields, DtmfMemoryKind::EchoLink, &self.echolink);
        let mut replacements = Vec::new();
        for first in [DTMF_CODES, DTMF_NAMES, ECHOLINK_CODES] {
            let page = regions::writable_page_for(Address::new(first)?).ok_or(
                McpError::PageNotWritable {
                    address: first,
                    len: 1,
                },
            )?;
            let [start, end] = [page.address().as_u32(), page.end()];
            let expected = chain::stored_bytes(image, start, end - start);
            let mut replacement = expected.clone();
            for (address, bytes) in &fields {
                if (start..end).contains(address) {
                    chain::put(&mut replacement, address - start, bytes);
                }
            }
            if replacement != expected {
                replacements.push(PageReplacement::new(page, &expected, &replacement)?);
            }
        }
        Ok(replacements)
    }
}

/// One memory's channels decoded from `image`.
fn read_table<const N: usize>(
    image: &MemoryImage,
    kind: DtmfMemoryKind,
) -> Result<[Option<DtmfEntry<N>>; DTMF_MEMORY_CHANNELS], DtmfMemoryError> {
    let mut table = [None; DTMF_MEMORY_CHANNELS];
    for (slot, channel) in table.iter_mut().zip(DtmfChannel::ALL) {
        let name = chain::stored_field(image, kind.name_address(channel));
        let code = chain::stored_field(image, kind.code_address(channel));
        *slot = DtmfEntry::from_bytes(name, code).map_err(|source| DtmfMemoryError::Entry {
            kind,
            channel,
            source: Box::new(source),
        })?;
    }
    Ok(table)
}

/// Append the address and stored bytes of every field of one memory.
fn push_fields<const N: usize>(
    fields: &mut Vec<(u32, Vec<u8>)>,
    kind: DtmfMemoryKind,
    table: &[Option<DtmfEntry<N>>; DTMF_MEMORY_CHANNELS],
) {
    for (channel, entry) in DtmfChannel::ALL.into_iter().zip(table) {
        let (name, code) = entry.map_or(([0; N], [0; N]), |entry| (entry.name, entry.code));
        fields.push((kind.name_address(channel), name.to_vec()));
        fields.push((kind.code_address(channel), code.to_vec()));
    }
}

/// A channel, entry or stored memory the library refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DtmfMemoryError {
    /// A name is longer than its field or holds a character outside
    /// printable ASCII.
    #[error(
        "name {name:?} is longer than {capacity} bytes or holds a character outside printable ASCII"
    )]
    InvalidName {
        /// The rejected name.
        name: String,
        /// The field's size in bytes.
        capacity: usize,
    },
    /// A code is longer than its field or holds a character other than `0`
    /// to `9`, `A` to `D`, `*`, `#` and space.
    #[error(
        "code {code:?} is longer than {capacity} characters or holds one other than 0-9, A-D, *, # and space"
    )]
    InvalidCode {
        /// The rejected code.
        code: String,
        /// The field's size in bytes.
        capacity: usize,
    },
    /// A channel with neither a name nor a code; clear the channel instead.
    #[error("a DTMF or EchoLink memory channel needs a name or a code")]
    Empty,
    /// A stored field is not zero-filled text of its character set.
    #[error("stored field {bytes:02X?} is not zero-filled text of its character set")]
    Malformed {
        /// The stored bytes.
        bytes: Vec<u8>,
    },
    /// A channel number above 9.
    #[error("channel {channel} is outside CH0 to CH9")]
    ChannelOutOfRange {
        /// The rejected number.
        channel: u8,
    },
    /// A channel of a stored memory did not decode.
    #[error("{kind} {channel}: {source}")]
    Entry {
        /// The memory.
        kind: DtmfMemoryKind,
        /// The channel.
        channel: DtmfChannel,
        /// The channel's error.
        source: Box<Self>,
    },
}
