use super::*;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fixture<T> = Result<T, Box<dyn std::error::Error>>;

fn place(image: &mut [u8], address: u32, data: &[u8]) -> TestResult {
    let start = address as usize;
    image
        .get_mut(start..start + data.len())
        .ok_or("inside the image")?
        .copy_from_slice(data);
    Ok(())
}

/// An image whose list holds the empty-slot form in every slot and no ends;
/// every other byte is `5A`.
fn empty_list() -> Fixture<Vec<u8>> {
    let mut image = vec![0x5A; IMAGE_LENGTH];
    for index in 0..CALLSIGN_LIST_CAPACITY {
        place(&mut image, CallsignSlot::new(index)?.address(), &EMPTY_SLOT)?;
    }
    place(&mut image, CALLSIGN_LIST_ENDS_ADDRESS, &[0xFF; 4])?;
    Ok(image)
}

fn entry(name: &str, callsign: &str, memo: &str) -> Fixture<CallsignEntry> {
    Ok(CallsignEntry::new(
        StoredText::new(name)?,
        StoredCallsign::new(callsign)?,
        StoredText::new(memo)?,
    )?)
}

fn linked(entry: &CallsignEntry, previous: Option<u16>, next: Option<u16>) -> [u8; 64] {
    let mut linked = entry.clone();
    linked.set_links(previous, next);
    linked.to_bytes()
}

/// An image holding `entries` in their slots and `ends`.
fn stored(entries: &[(u16, [u8; 64])], ends: [Option<u16>; 2]) -> Fixture<MemoryImage> {
    let mut image = empty_list()?;
    for (index, bytes) in entries {
        place(&mut image, CallsignSlot::new(*index)?.address(), bytes)?;
    }
    let [first, last] = ends;
    place(
        &mut image,
        CALLSIGN_LIST_ENDS_ADDRESS,
        &chain::ends_bytes(first, last),
    )?;
    Ok(MemoryImage::from_bytes(image)?)
}

fn applied(image: &MemoryImage, replacements: &[PageReplacement]) -> Fixture<MemoryImage> {
    let mut raw = image.as_bytes().to_vec();
    for replacement in replacements {
        place(
            &mut raw,
            replacement.page().address().as_u32(),
            replacement.replacement(),
        )?;
    }
    Ok(MemoryImage::from_bytes(raw)?)
}

#[test]
fn an_empty_table_reads_as_an_empty_list_and_stores_unchanged() -> TestResult {
    let image = stored(&[], [None, None])?;
    let list = CallsignList::read(&image)?;
    assert!(list.is_empty());
    assert_eq!(list.page_replacements(&image)?, Vec::new());
    assert_eq!(
        CallsignList::from_entries(Vec::new())?.page_replacements(&image)?,
        Vec::new()
    );
    Ok(())
}

#[test]
fn entries_decode_in_chain_order_with_their_fields() -> TestResult {
    let reflector = entry("REF001 C", "REF001CL", "Link reflector 1 C")?;
    let cq = entry("CQ", "CQCQCQ", "")?;
    let friend = entry("Jürgen", "DL1ABC", "Thirty-two characters of memo!!!")?;
    let image = stored(
        &[
            (2, linked(&reflector, None, Some(0))),
            (0, linked(&cq, Some(2), Some(5))),
            (5, linked(&friend, Some(0), None)),
        ],
        [Some(2), Some(5)],
    )?;
    let decoded = CallsignList::read(&image)?;
    let order: Vec<u16> = decoded.iter().map(|(slot, _)| slot.index()).collect();
    assert_eq!(order, [2, 0, 5], "display order follows the chain");
    let head = decoded.get(CallsignSlot::new(2)?).ok_or("slot 2")?;
    assert_eq!(head.name().to_string(), "REF001 C");
    assert_eq!(head.callsign().text(), "REF001CL");
    assert_eq!(head.memo().to_string(), "Link reflector 1 C");
    let tail = decoded.get(CallsignSlot::new(5)?).ok_or("slot 5")?;
    assert_eq!(tail.name().to_string(), "Jürgen");
    assert_eq!(tail.memo().to_string(), "Thirty-two characters of memo!!!");
    assert_eq!((tail.previous(), tail.next()), (Some(0), None));
    assert_eq!(decoded.page_replacements(&image)?, Vec::new());
    Ok(())
}

#[test]
fn a_chain_that_misses_or_misnames_a_slot_is_refused() -> TestResult {
    let one = entry("One", "W1AW", "")?;
    let two = entry("Two", "K1ABC", "")?;
    let cases = [
        (
            stored(&[(0, linked(&one, None, Some(1)))], [Some(0), Some(0)])?,
            CallsignListError::ChainBroken { index: 1 },
        ),
        (
            stored(
                &[
                    (0, linked(&one, None, Some(1))),
                    (1, linked(&two, Some(3), None)),
                ],
                [Some(0), Some(1)],
            )?,
            CallsignListError::ChainBroken { index: 1 },
        ),
        (
            stored(
                &[
                    (0, linked(&one, Some(1), Some(1))),
                    (1, linked(&two, Some(0), Some(0))),
                ],
                [Some(0), Some(1)],
            )?,
            CallsignListError::ChainBroken { index: 0 },
        ),
        (
            stored(&[(0, linked(&one, None, Some(300)))], [Some(0), Some(0)])?,
            CallsignListError::ChainBroken { index: 300 },
        ),
        (
            stored(
                &[
                    (0, linked(&one, None, Some(1))),
                    (1, linked(&two, Some(0), None)),
                ],
                [Some(0), Some(0)],
            )?,
            CallsignListError::EndsMismatch {
                chain: Some(1),
                stored: Some(0),
            },
        ),
        (
            stored(
                &[(0, linked(&one, None, None)), (7, linked(&two, None, None))],
                [Some(0), Some(0)],
            )?,
            CallsignListError::UnchainedEntry { index: 7 },
        ),
    ];
    for (image, expected) in cases {
        assert_eq!(CallsignList::read(&image), Err(expected.clone()));
    }
    Ok(())
}

#[test]
fn a_stored_list_is_compact_from_slot_zero_and_keeps_every_other_byte() -> TestResult {
    let mut raw = empty_list()?;
    let ends_page = CALLSIGN_LIST_ENDS_ADDRESS - CALLSIGN_LIST_ENDS_ADDRESS % 256;
    place(&mut raw, ends_page + 0xF0, &[0x00, 0x00, 0xAE, 0x05])?;
    let captured = MemoryImage::from_bytes(raw)?;
    let entries = vec![
        entry("Alpha", "W1AW", "first")?,
        entry("Bravo", "K1ABC", "")?,
    ];
    let list = CallsignList::from_entries(entries.clone())?;
    let replacements = list.page_replacements(&captured)?;
    let pages: Vec<u32> = replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [CALLSIGN_LIST_ADDRESS, ends_page]);
    let stored = applied(&captured, &replacements)?;
    let reread = CallsignList::read(&stored)?;
    assert_eq!(reread, list);
    let decoded: Vec<(u16, StoredCallsign)> = reread
        .iter()
        .map(|(slot, stored)| (slot.index(), stored.callsign()))
        .collect();
    let expected: Vec<(u16, StoredCallsign)> = (0..)
        .zip(entries.iter().map(CallsignEntry::callsign))
        .collect();
    assert_eq!(decoded, expected);
    let ends = stored
        .as_bytes()
        .get(ends_page as usize + 0xF0..ends_page as usize + 0xF8)
        .ok_or("the ends")?;
    assert_eq!(
        ends,
        [0x00, 0x00, 0xAE, 0x05, 0x00, 0x00, 0x01, 0x00],
        "the repeater ends before the callsign ends are kept"
    );
    let changed = stored
        .as_bytes()
        .iter()
        .zip(captured.as_bytes())
        .filter(|(after, before)| after != before)
        .count();
    assert!(changed <= 2 * 64 + 4, "{changed} bytes changed");
    let cleared = CallsignList::from_entries(Vec::new())?.page_replacements(&stored)?;
    assert_eq!(applied(&stored, &cleared)?, captured, "clearing restores");
    Ok(())
}

#[test]
fn an_entry_keeps_its_unnamed_bytes_and_each_setter_its_own_field() -> TestResult {
    let mut bytes = linked(&entry("Name", "W1AW", "memo")?, None, None);
    place(&mut bytes, 56, &[0x12, 0x34, 0x56])?;
    let mut stored = CallsignEntry::from_bytes(bytes)?.ok_or("an entry")?;
    assert_eq!(stored.to_bytes(), bytes, "every byte is kept");
    stored.set_memo(StoredText::new("new memo")?);
    let changed: Vec<usize> = stored
        .to_bytes()
        .iter()
        .zip(bytes)
        .enumerate()
        .filter(|(_, (after, before))| *after != before)
        .map(|(offset, _)| offset)
        .collect();
    assert!(
        changed.iter().all(|offset| (24..56).contains(offset)),
        "{changed:?}"
    );
    let fresh = entry("Name", "W1AW", "memo")?.to_bytes();
    assert_eq!(
        fresh.get(56..64),
        Some(&[0xFF, 0xFF, 0xFF, 0, 0xFF, 0xFF, 0xFF, 0xFF][..]),
        "a new entry takes the empty form's unnamed bytes and a zero state byte"
    );
    Ok(())
}

#[test]
fn malformed_entries_empty_slots_and_overfull_lists_are_refused() -> TestResult {
    assert_eq!(CallsignEntry::from_bytes(EMPTY_SLOT)?, None);
    let mut bad_name = linked(&entry("Name", "W1AW", "")?, None, None);
    place(&mut bad_name, 0, b"tab\t")?;
    assert!(matches!(
        CallsignEntry::from_bytes(bad_name),
        Err(CallsignListError::Text(StoredTextError::Malformed { .. }))
    ));
    let mut bad_callsign = linked(&entry("Name", "W1AW", "")?, None, None);
    place(&mut bad_callsign, 16, b"W1\0AW")?;
    assert!(matches!(
        CallsignEntry::from_bytes(bad_callsign),
        Err(CallsignListError::MalformedCallsign { .. })
    ));
    let mut image = stored(&[(4, bad_callsign)], [Some(4), Some(4)])?
        .as_bytes()
        .to_vec();
    place(&mut image, CALLSIGN_LIST_ENDS_ADDRESS, &[4, 0, 4, 0])?;
    assert!(matches!(
        CallsignList::read(&MemoryImage::from_bytes(image)?),
        Err(CallsignListError::Entry { slot, .. }) if slot.index() == 4
    ));
    let overfull = vec![entry("Name", "W1AW", "")?; 301];
    assert_eq!(
        CallsignList::from_entries(overfull),
        Err(CallsignListError::TooManyEntries { count: 301 })
    );
    assert_eq!(
        CallsignSlot::new(300),
        Err(CallsignListError::SlotOutOfRange { index: 300 })
    );
    assert_eq!(CallsignSlot::new(299)?.address(), 0x2_5000 + 299 * 64);
    Ok(())
}
