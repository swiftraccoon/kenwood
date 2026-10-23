use super::*;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The image spans of the stored fields, written out: the DTMF codes and
/// names, then the `EchoLink` codes and names.
const SPANS: [(usize, usize); 4] = [(0x40, 0xE0), (0x100, 0x1A0), (0x200, 0x250), (0x250, 0x2A0)];

/// A full image of `0x42` whose memories are all zero, as a radio with no
/// stored channels holds them.
fn empty_memories() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    for (start, end) in SPANS {
        image
            .get_mut(start..end)
            .ok_or("the memories inside the image")?
            .fill(0);
    }
    Ok(image)
}

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

fn channel(index: u8) -> Result<DtmfChannel, DtmfMemoryError> {
    DtmfChannel::new(index)
}

/// The addresses of every image byte a replacement changes.
fn changed(replacement: &PageReplacement) -> Vec<usize> {
    let start = replacement.page().address().as_usize();
    replacement
        .expected()
        .iter()
        .zip(replacement.replacement())
        .enumerate()
        .filter(|(_, (before, after))| before != after)
        .map(|(offset, _)| start + offset)
        .collect()
}

#[test]
fn the_memories_sit_at_the_programs_addresses() -> TestResult {
    let [first, last] = [channel(0)?, channel(9)?];
    let dtmf = DtmfMemoryKind::Dtmf;
    let echolink = DtmfMemoryKind::EchoLink;
    assert_eq!(
        [dtmf.code_address(first), dtmf.code_address(last)],
        [0x40, 0xD0]
    );
    assert_eq!(
        [dtmf.name_address(first), dtmf.name_address(last)],
        [0x100, 0x190]
    );
    assert_eq!(
        [echolink.code_address(first), echolink.code_address(last)],
        [0x200, 0x248]
    );
    assert_eq!(
        [echolink.name_address(first), echolink.name_address(last)],
        [0x250, 0x298]
    );
    assert_eq!([dtmf.field_size(), echolink.field_size()], [16, 8]);
    assert_eq!(
        [dtmf.to_string(), echolink.to_string()],
        ["DTMF memory", "EchoLink memory"]
    );
    Ok(())
}

#[test]
fn channels_are_ch0_to_ch9() -> TestResult {
    let indexes: Vec<u8> = DtmfChannel::ALL
        .into_iter()
        .map(DtmfChannel::index)
        .collect();
    assert_eq!(indexes, (0..10).collect::<Vec<u8>>());
    assert_eq!(channel(9)?.to_string(), "CH9");
    assert_eq!(
        DtmfChannel::new(10),
        Err(DtmfMemoryError::ChannelOutOfRange { channel: 10 })
    );
    Ok(())
}

#[test]
fn an_entry_round_trips_through_its_stored_form() -> TestResult {
    let entry = DtmfMemoryEntry::new("Home", "*12 3#")?;
    assert_eq!(entry.name_bytes(), b"Home\0\0\0\0\0\0\0\0\0\0\0\0");
    assert_eq!(entry.code_bytes(), b"*12 3#\0\0\0\0\0\0\0\0\0\0");
    assert_eq!(
        DtmfMemoryEntry::from_bytes(*entry.name_bytes(), *entry.code_bytes())?,
        Some(entry)
    );
    assert_eq!([entry.name(), entry.code()], ["Home", "*12 3#"]);
    assert_eq!(entry.to_string(), "\"*12 3#\" \"Home\"");
    let full = DtmfMemoryEntry::new("Sixteen bytes!!!", "0123456789ABCD*#")?;
    assert_eq!(
        DtmfMemoryEntry::from_bytes(*full.name_bytes(), *full.code_bytes())?,
        Some(full)
    );
    let node = EchoLinkMemoryEntry::new("W1AW-L", "12345678")?;
    assert_eq!(node.code_bytes(), b"12345678");
    Ok(())
}

#[test]
fn a_channel_needs_a_name_or_a_code() -> TestResult {
    assert_eq!(DtmfMemoryEntry::new("", ""), Err(DtmfMemoryError::Empty));
    let unnamed = DtmfMemoryEntry::new("", "911")?;
    assert_eq!([unnamed.name(), unnamed.code()], ["", "911"]);
    assert_eq!(
        DtmfMemoryEntry::from_bytes([0; 16], *unnamed.code_bytes())?,
        Some(unnamed)
    );
    let codeless = EchoLinkMemoryEntry::new("NET", "")?;
    assert_eq!(
        EchoLinkMemoryEntry::from_bytes(*codeless.name_bytes(), [0; 8])?,
        Some(codeless)
    );
    assert_eq!(DtmfMemoryEntry::from_bytes([0; 16], [0; 16])?, None);
    Ok(())
}

#[test]
fn new_admits_dtmf_digits_and_printable_names_within_the_field() {
    for code in ["12a", "E", "1-2", "123456789", "\u{7F}"] {
        assert_eq!(
            EchoLinkMemoryEntry::new("X", code),
            Err(DtmfMemoryError::InvalidCode {
                code: code.to_owned(),
                capacity: 8
            })
        );
    }
    for name in ["Nine byte", "Ünï", "tab\there"] {
        assert_eq!(
            EchoLinkMemoryEntry::new(name, "1"),
            Err(DtmfMemoryError::InvalidName {
                name: name.to_owned(),
                capacity: 8
            })
        );
    }
    assert!(DtmfMemoryEntry::new("Nine byte", "123456789").is_ok());
    assert!(DtmfMemoryEntry::new("~ Lower case ok", "#").is_ok());
}

#[test]
fn stored_fields_must_be_zero_filled_text_of_their_character_set() -> TestResult {
    let named = DtmfMemoryEntry::new("AB", "12")?;
    let mut gap = *named.name_bytes();
    gap[3] = b'C';
    let mut lowercase = *named.code_bytes();
    lowercase[0] = b'a';
    let mut control = *named.name_bytes();
    control[1] = 0x07;
    for (name, code) in [
        (gap, *named.code_bytes()),
        (*named.name_bytes(), lowercase),
        (control, *named.code_bytes()),
        ([0xFF; 16], [0; 16]),
        ([0; 16], [0xFF; 16]),
    ] {
        assert!(
            matches!(
                DtmfMemoryEntry::from_bytes(name, code),
                Err(DtmfMemoryError::Malformed { .. })
            ),
            "{name:02X?} {code:02X?}"
        );
    }
    Ok(())
}

#[test]
fn read_places_each_stored_channel_and_an_edit_rewrites_only_its_fields() -> TestResult {
    let mut bytes = empty_memories()?;
    put(&mut bytes, 0x70, b"5551212")?;
    put(&mut bytes, 0x130, b"Office")?;
    put(&mut bytes, 0x248, b"9999")?;
    put(&mut bytes, 0x298, b"TEST")?;
    let image = MemoryImage::from_bytes(bytes)?;
    let mut memories = DtmfMemories::read(&image)?;
    assert_eq!(
        memories.dtmf(channel(3)?),
        Some(&DtmfMemoryEntry::new("Office", "5551212")?)
    );
    assert_eq!(
        memories.echolink(channel(9)?),
        Some(&EchoLinkMemoryEntry::new("TEST", "9999")?)
    );
    let unused = DtmfChannel::ALL
        .iter()
        .filter(|&&slot| memories.dtmf(slot).is_none() && memories.echolink(slot).is_none())
        .count();
    assert_eq!(unused, 8);
    assert_eq!(memories.page_replacements(&image)?, []);

    memories.set_echolink(channel(0)?, Some(EchoLinkMemoryEntry::new("K", "7")?));
    let replacements = memories.page_replacements(&image)?;
    let [echolink] = replacements.as_slice() else {
        return Err(format!("expected one page, got {}", replacements.len()).into());
    };
    assert_eq!(
        [
            echolink.page().address().as_u32(),
            echolink.page().len().try_into()?
        ],
        [0x200, 256]
    );
    assert_eq!(changed(echolink), [0x200, 0x250]);

    memories.set_echolink(channel(0)?, None);
    memories.set_dtmf(channel(9)?, Some(DtmfMemoryEntry::new("Z", "0")?));
    let replacements = memories.page_replacements(&image)?;
    let pages: Vec<(u32, usize)> = replacements
        .iter()
        .map(|replacement| {
            (
                replacement.page().address().as_u32(),
                replacement.page().len(),
            )
        })
        .collect();
    assert_eq!(pages, [(0x38, 200), (0x100, 160)]);
    let addresses: Vec<usize> = replacements.iter().flat_map(changed).collect();
    assert_eq!(addresses, [0xD0, 0x190]);

    memories.set_dtmf(channel(9)?, None);
    memories.set_dtmf(channel(3)?, None);
    memories.set_echolink(channel(9)?, None);
    let cleared: Vec<usize> = memories
        .page_replacements(&image)?
        .iter()
        .flat_map(changed)
        .collect();
    let expected: Vec<usize> = (0x70..0x77)
        .chain(0x130..0x136)
        .chain(0x248..0x24C)
        .chain(0x298..0x29C)
        .collect();
    assert_eq!(cleared, expected);
    Ok(())
}

#[test]
fn a_malformed_stored_channel_names_its_memory_and_channel() -> TestResult {
    let mut bytes = empty_memories()?;
    put(&mut bytes, 0x220, b"12x")?;
    let image = MemoryImage::from_bytes(bytes)?;
    assert!(matches!(
        DtmfMemories::read(&image),
        Err(DtmfMemoryError::Entry {
            kind: DtmfMemoryKind::EchoLink,
            channel: DtmfChannel(4),
            ..
        })
    ));
    Ok(())
}
