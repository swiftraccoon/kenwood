use super::fixtures::{AKIHABARA, BUENOS_AIRES, OZONDJISSE, REYKJAVIK, UNUSED, bytes, linked};
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fixture<T> = Result<T, Box<dyn std::error::Error>>;

const NO_ENDS: (Option<u16>, Option<u16>) = (None, None);

fn place(image: &mut [u8], address: u32, data: &[u8]) -> TestResult {
    let start = address as usize;
    image
        .get_mut(start..start + data.len())
        .ok_or("inside the image")?
        .copy_from_slice(data);
    Ok(())
}

fn store_ends(
    image: &mut [u8],
    kind: RepeaterListKind,
    ends: (Option<u16>, Option<u16>),
) -> TestResult {
    let stored = <[Option<u16>; 2]>::from(ends).map(|end| end.unwrap_or(u16::MAX).to_le_bytes());
    place(image, kind.ends_address(), &stored.concat())
}

/// An image whose two lists hold `records` of `kind` at the given slots,
/// with `ends` stored for `kind` and empty ends for the other list.
fn image(
    kind: RepeaterListKind,
    records: &[(u16, [u8; REPEATER_RECORD_SIZE])],
    ends: (Option<u16>, Option<u16>),
) -> Fixture<MemoryImage> {
    let mut image = vec![0xFF; crate::types::IMAGE_LENGTH];
    for list in [RepeaterListKind::Repeater, RepeaterListKind::Hotspot] {
        for index in 0..list.capacity() {
            place(
                &mut image,
                RepeaterSlot::new(list, index)?.address(),
                &bytes(UNUSED)?,
            )?;
        }
        store_ends(&mut image, list, NO_ENDS)?;
    }
    for (index, record) in records {
        place(
            &mut image,
            RepeaterSlot::new(kind, *index)?.address(),
            record,
        )?;
    }
    store_ends(&mut image, kind, ends)?;
    Ok(MemoryImage::from_bytes(image)?)
}

fn order(list: &RepeaterList) -> Vec<u16> {
    list.iter().map(|(slot, _)| slot.index()).collect()
}

#[test]
fn the_geometry_matches_the_stored_tables() -> TestResult {
    let repeater = RepeaterListKind::Repeater;
    let hotspot = RepeaterListKind::Hotspot;
    assert_eq!((repeater.capacity(), hotspot.capacity()), (1500, 30));
    assert_eq!(
        (repeater.records_address(), repeater.ends_address()),
        (0x2_A000, 0x4_93F0)
    );
    assert_eq!(
        (hotspot.records_address(), hotspot.ends_address()),
        (0x4_9400, 0x4_9DF0)
    );
    for (kind, index, address) in [
        (repeater, 0, 0x2_A000),
        (repeater, 1, 0x2_A050),
        (repeater, 2, 0x2_A0A0),
        (repeater, 3, 0x2_A100),
        (repeater, 1499, 0x4_93A0),
        (hotspot, 0, 0x4_9400),
        (hotspot, 29, 0x4_9DA0),
    ] {
        assert_eq!(RepeaterSlot::new(kind, index)?.address(), address);
    }
    assert!(matches!(
        RepeaterSlot::new(repeater, 1500),
        Err(RepeaterListError::SlotOutOfRange { .. })
    ));
    assert!(RepeaterSlot::new(hotspot, 30).is_err());
    assert_eq!(RepeaterSlot::new(hotspot, 7)?.to_string(), "hotspot slot 7");
    Ok(())
}

#[test]
fn the_list_reads_in_chain_order_from_the_stored_first_slot() -> TestResult {
    let kind = RepeaterListKind::Repeater;
    let captured = image(
        kind,
        &[
            (0, linked(AKIHABARA, None, Some(1))?),
            (1, linked(REYKJAVIK, Some(0), Some(2))?),
            (2, linked(OZONDJISSE, Some(1), None)?),
        ],
        (Some(0), Some(2)),
    )?;
    assert_eq!(order(&RepeaterList::read(&captured, kind)?), [0, 1, 2]);

    let reordered = image(
        kind,
        &[
            (2, linked(AKIHABARA, None, Some(0))?),
            (0, linked(REYKJAVIK, Some(2), Some(1))?),
            (1, linked(OZONDJISSE, Some(0), None)?),
        ],
        (Some(2), Some(1)),
    )?;
    let list = RepeaterList::read(&reordered, kind)?;
    assert_eq!(
        order(&list),
        [2, 0, 1],
        "the chain, not the slot number, orders the list"
    );
    assert_eq!(list.len(), 3);
    assert_eq!(list.kind(), kind);
    let first = list.get(RepeaterSlot::new(kind, 2)?).ok_or("slot 2")?;
    assert_eq!(first.name().to_string(), "Akihabara");
    assert!(list.get(RepeaterSlot::new(kind, 3)?).is_none());
    Ok(())
}

#[test]
fn empty_lists_read_empty() -> TestResult {
    let blank = image(RepeaterListKind::Repeater, &[], NO_ENDS)?;
    for kind in [RepeaterListKind::Repeater, RepeaterListKind::Hotspot] {
        assert!(RepeaterList::read(&blank, kind)?.is_empty());
    }
    Ok(())
}

#[test]
fn the_hotspot_list_reads_its_own_table() -> TestResult {
    let kind = RepeaterListKind::Hotspot;
    let captured = image(
        kind,
        &[(29, linked(BUENOS_AIRES, None, None)?)],
        (Some(29), Some(29)),
    )?;
    assert_eq!(order(&RepeaterList::read(&captured, kind)?), [29]);
    assert!(RepeaterList::read(&captured, RepeaterListKind::Repeater)?.is_empty());
    Ok(())
}

#[test]
fn a_broken_chain_or_disagreeing_ends_is_refused() -> TestResult {
    let kind = RepeaterListKind::Hotspot;
    let cases = [
        ("the stored first slot is empty", vec![], (Some(0), Some(0))),
        (
            "the next link reaches an empty slot",
            vec![(0, linked(AKIHABARA, None, Some(1))?)],
            (Some(0), Some(1)),
        ),
        (
            "a back link names another slot",
            vec![
                (0, linked(AKIHABARA, None, Some(1))?),
                (1, linked(REYKJAVIK, Some(5), None)?),
            ],
            (Some(0), Some(1)),
        ),
        (
            "a link leaves the table",
            vec![(0, linked(AKIHABARA, None, Some(30))?)],
            (Some(0), Some(30)),
        ),
        (
            "the chain returns to its start",
            vec![
                (0, linked(AKIHABARA, None, Some(1))?),
                (1, linked(REYKJAVIK, Some(0), Some(0))?),
            ],
            (Some(0), Some(1)),
        ),
    ];
    for (case, records, ends) in cases {
        let result = RepeaterList::read(&image(kind, &records, ends)?, kind);
        assert!(
            matches!(result, Err(RepeaterListError::ChainBroken { .. })),
            "{case}: {result:?}"
        );
    }

    let stale_last = image(
        kind,
        &[(0, linked(AKIHABARA, None, None)?)],
        (Some(0), Some(4)),
    )?;
    let result = RepeaterList::read(&stale_last, kind);
    assert!(
        matches!(result, Err(RepeaterListError::EndsMismatch { .. })),
        "{result:?}"
    );
    let orphan = image(
        kind,
        &[
            (0, linked(AKIHABARA, None, None)?),
            (3, linked(REYKJAVIK, None, None)?),
        ],
        (Some(0), Some(0)),
    )?;
    let result = RepeaterList::read(&orphan, kind);
    assert!(
        matches!(result, Err(RepeaterListError::UnchainedRecord { .. })),
        "{result:?}"
    );

    let mut zone = linked(AKIHABARA, None, None)?;
    *zone.get_mut(66).ok_or("byte 66")? = 200;
    let result = RepeaterList::read(&image(kind, &[(4, zone)], (Some(4), Some(4)))?, kind);
    assert!(
        matches!(
            &result,
            Err(RepeaterListError::Record { slot, .. }) if slot.index() == 4
        ),
        "a field error names its slot: {result:?}"
    );
    Ok(())
}

#[test]
fn the_name_tables_resolve_every_index() -> TestResult {
    let mut raw = image(RepeaterListKind::Repeater, &[], NO_ENDS)?.into_bytes();
    for (address, name) in [
        (REPEATER_REGION_NAMES_ADDRESS, "Asia"),
        (REPEATER_REGION_NAMES_ADDRESS + 5 * 16, "Oceania"),
        (REPEATER_COUNTRY_NAMES_ADDRESS, "Japan"),
        (REPEATER_COUNTRY_NAMES_ADDRESS + 149 * 16, "Last country"),
        (REPEATER_GROUP_NAMES_ADDRESS, "JA1 : Kanto"),
        (
            REPEATER_GROUP_NAMES_ADDRESS + 299 * 16,
            "\u{15a}wi\u{119}tokrzyskie",
        ),
    ] {
        place(
            &mut raw,
            address,
            StoredText::<REPEATER_TEXT_SIZE>::new(name)?.as_bytes(),
        )?;
    }
    place(&mut raw, REPEATER_COUNTRY_NAMES_ADDRESS + 16, &[0; 16])?;
    let names = RepeaterNames::read(&MemoryImage::from_bytes(raw)?)?;
    assert_eq!(names.region(RegionIndex::new(0)?).to_string(), "Asia");
    assert_eq!(names.region(RegionIndex::new(5)?).to_string(), "Oceania");
    assert_eq!(names.country(CountryIndex::new(0)?).to_string(), "Japan");
    assert_eq!(
        names.country(CountryIndex::new(149)?).to_string(),
        "Last country"
    );
    assert!(names.country(CountryIndex::new(1)?).is_empty());
    assert_eq!(names.group(GroupIndex::new(0)?).to_string(), "JA1 : Kanto");
    assert_eq!(
        names.group(GroupIndex::new(299)?).to_string(),
        "\u{15a}wi\u{119}tokrzyskie"
    );
    Ok(())
}

#[test]
fn an_in_place_edit_rewrites_only_its_record_bytes() -> TestResult {
    let kind = RepeaterListKind::Repeater;
    let captured = image(
        kind,
        &[
            (0, linked(AKIHABARA, None, Some(1))?),
            (1, linked(REYKJAVIK, Some(0), Some(2))?),
            (2, linked(OZONDJISSE, Some(1), None)?),
        ],
        (Some(0), Some(2)),
    )?;
    let list = RepeaterList::read(&captured, kind)?;
    for (index, record_offset) in [(0, 0), (2, 160)] {
        let slot = RepeaterSlot::new(kind, index)?;
        let mut edited = list.get(slot).ok_or("populated")?.clone();
        edited.set_frequency(RepeaterFrequency::new(edited.frequency().hertz() + 10_000)?);
        let replacement = edited.page_replacement(&captured, slot)?;
        assert_eq!(replacement.page().address().as_u32(), 0x2_A000);
        let page = captured.as_bytes().get(0x2_A000..0x2_A100).ok_or("page")?;
        assert_eq!(replacement.expected(), page);
        let changed: Vec<usize> = replacement
            .replacement()
            .iter()
            .zip(page)
            .enumerate()
            .filter(|(_, (new, old))| new != old)
            .map(|(offset, _)| offset - record_offset)
            .collect();
        assert!(
            !changed.is_empty() && changed.iter().all(|offset| (48..52).contains(offset)),
            "slot {index}: {changed:?}"
        );
    }
    Ok(())
}

#[test]
fn an_edit_of_an_empty_slot_or_with_other_links_is_refused() -> TestResult {
    let kind = RepeaterListKind::Repeater;
    let captured = image(
        kind,
        &[
            (0, linked(AKIHABARA, None, Some(1))?),
            (1, linked(REYKJAVIK, Some(0), None)?),
        ],
        (Some(0), Some(1)),
    )?;
    let akihabara =
        RepeaterRecord::from_bytes(linked(AKIHABARA, None, Some(1))?)?.ok_or("populated")?;
    let result = akihabara.page_replacement(&captured, RepeaterSlot::new(kind, 2)?);
    assert!(
        matches!(
            result,
            Err(crate::Error::RepeaterList(
                RepeaterListError::EmptySlot { .. }
            ))
        ),
        "{result:?}"
    );
    let result = akihabara.page_replacement(&captured, RepeaterSlot::new(kind, 1)?);
    assert!(
        matches!(
            result,
            Err(crate::Error::RepeaterList(
                RepeaterListError::LinksChanged { .. }
            ))
        ),
        "a record carrying another slot's links: {result:?}"
    );
    Ok(())
}

/// `image` with every replacement's bytes stored.
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

fn three_records(kind: RepeaterListKind) -> Fixture<MemoryImage> {
    image(
        kind,
        &[
            (0, linked(AKIHABARA, None, Some(1))?),
            (1, linked(REYKJAVIK, Some(0), Some(2))?),
            (2, linked(OZONDJISSE, Some(1), None)?),
        ],
        (Some(0), Some(2)),
    )
}

#[test]
fn re_encoding_a_stored_list_changes_no_page() -> TestResult {
    let kind = RepeaterListKind::Repeater;
    let captured = three_records(kind)?;
    let list = RepeaterList::read(&captured, kind)?;
    assert!(list.page_replacements(&captured)?.is_empty());
    let rebuilt = RepeaterList::from_records(
        kind,
        list.iter().map(|(_, record)| record.clone()).collect(),
    )?;
    assert_eq!(rebuilt, list);
    assert!(rebuilt.page_replacements(&captured)?.is_empty());

    let reordered = image(
        kind,
        &[
            (2, linked(AKIHABARA, None, Some(0))?),
            (0, linked(REYKJAVIK, Some(2), None)?),
        ],
        (Some(2), Some(0)),
    )?;
    let list = RepeaterList::read(&reordered, kind)?;
    assert!(
        list.page_replacements(&reordered)?.is_empty(),
        "a read list stores each record back in its own slot"
    );
    Ok(())
}

#[test]
fn a_rebuilt_list_is_stored_compact_in_its_new_order() -> TestResult {
    let kind = RepeaterListKind::Hotspot;
    let captured = three_records(kind)?;
    let list = RepeaterList::read(&captured, kind)?;
    let records = [2, 0]
        .into_iter()
        .map(|index| {
            let slot = RepeaterSlot::new(kind, index)?;
            Ok(list.get(slot).ok_or("populated")?.clone())
        })
        .collect::<Fixture<Vec<_>>>()?;
    let replacements = RepeaterList::from_records(kind, records)?.page_replacements(&captured)?;
    let pages: Vec<u32> = replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(
        pages,
        [0x4_9400, 0x4_9D00],
        "the records page and the ends page"
    );

    let stored = applied(&captured, &replacements)?;
    let reread = RepeaterList::read(&stored, kind)?;
    assert_eq!(order(&reread), [0, 1]);
    let names: Vec<String> = reread
        .iter()
        .map(|(_, record)| record.name().to_string())
        .collect();
    assert_eq!(names, ["Farm Ozondjisse", "Akihabara"]);
    assert_eq!(
        stored_record(&stored, RepeaterSlot::new(kind, 2)?),
        bytes(UNUSED)?,
        "the freed slot is stored in the empty-slot form"
    );
    let changed: Vec<usize> = captured
        .as_bytes()
        .iter()
        .zip(stored.as_bytes())
        .enumerate()
        .filter(|(_, (old, new))| old != new)
        .map(|(offset, _)| offset)
        .collect();
    assert!(
        changed
            .iter()
            .all(|offset| (0x4_9400..0x4_94F0).contains(offset)
                || (0x4_9DF0..0x4_9DF4).contains(offset)),
        "only the record slots and the ends change: {changed:X?}"
    );
    Ok(())
}

#[test]
fn a_list_beyond_its_capacity_is_refused() -> TestResult {
    let record = RepeaterRecord::from_bytes(bytes(AKIHABARA)?)?.ok_or("populated")?;
    let result = RepeaterList::from_records(RepeaterListKind::Hotspot, vec![record; 31]);
    assert!(
        matches!(
            result,
            Err(RepeaterListError::TooManyRecords { count: 31, .. })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn an_emptied_list_stores_empty_slots_and_no_ends() -> TestResult {
    let kind = RepeaterListKind::Repeater;
    let captured = three_records(kind)?;
    let replacements =
        RepeaterList::from_records(kind, Vec::new())?.page_replacements(&captured)?;
    let stored = applied(&captured, &replacements)?;
    assert!(RepeaterList::read(&stored, kind)?.is_empty());
    assert_eq!(
        chain::stored_ends(&stored, kind.ends_address()),
        [None, None]
    );
    assert_eq!(
        stored.as_bytes(),
        image(kind, &[], NO_ENDS)?.as_bytes(),
        "every slot holds the empty-slot form"
    );
    Ok(())
}

#[test]
fn the_ends_page_keeps_its_other_padding_bytes() -> TestResult {
    let kind = RepeaterListKind::Repeater;
    let mut raw = image(kind, &[], NO_ENDS)?.into_bytes();
    place(&mut raw, kind.ends_address() + 4, &[0x12, 0x34, 0x56, 0x78])?;
    let captured = MemoryImage::from_bytes(raw)?;
    let record = RepeaterRecord::from_bytes(bytes(AKIHABARA)?)?.ok_or("populated")?;
    let replacements =
        RepeaterList::from_records(kind, vec![record])?.page_replacements(&captured)?;
    let stored = applied(&captured, &replacements)?;
    let tail = kind.ends_address() as usize + 4;
    assert_eq!(
        stored.as_bytes().get(tail..tail + 4),
        Some(&[0x12, 0x34, 0x56, 0x78][..])
    );
    assert_eq!(
        chain::stored_ends(&stored, kind.ends_address()),
        [Some(0), Some(0)]
    );
    Ok(())
}

#[test]
fn voice_codes_are_read_beside_the_names() -> TestResult {
    let mut raw = image(RepeaterListKind::Repeater, &[], NO_ENDS)?.into_bytes();
    for (address, code) in [
        (0x3600, *b"JPN\0"),
        (0x3600 + 4 * 149, *b"ZAF\0"),
        (0x3A00, *b"JA1\0"),
        (0x3A00 + 4 * 299, *b"HB\0\0"),
        (0x3A00 + 4 * 7, [0; 4]),
        (0x3A00 + 4 * 8, *b"AB\xFF\xFF"),
    ] {
        place(&mut raw, address, &code)?;
    }
    let names = RepeaterNames::read(&MemoryImage::from_bytes(raw)?)?;
    let country = |index| CountryIndex::new(index).map(|index| names.country_voice(index).as_str());
    let group = |index| GroupIndex::new(index).map(|index| names.group_voice(index).as_str());
    assert_eq!(
        [country(0)?, country(149)?, country(1)?],
        ["JPN", "ZAF", ""]
    );
    assert_eq!(
        [group(0)?, group(299)?, group(7)?, group(8)?],
        ["JA1", "HB", "", "AB"]
    );
    assert!(names.country_voice(CountryIndex::new(1)?).is_empty());
    Ok(())
}

#[test]
fn voice_codes_are_uppercase_letters_and_digits() -> TestResult {
    assert_eq!(VoiceCode::new("JPN")?.as_bytes(), b"JPN\0");
    assert_eq!(VoiceCode::new("KH6L")?.as_bytes(), b"KH6L");
    assert_eq!(VoiceCode::new("")?, VoiceCode::EMPTY);
    for code in ["jpn", "ABCDE", "A-1", "É"] {
        assert_eq!(
            VoiceCode::new(code),
            Err(RepeaterListError::InvalidVoiceCode {
                code: code.to_owned()
            })
        );
    }
    for bytes in [*b"JP\0N", [0x07, 0, 0, 0], *b"jpn\0", *b"\0\xFFAB"] {
        assert!(VoiceCode::from_bytes(bytes).is_err(), "{bytes:02X?}");
    }
    assert!(VoiceCode::from_bytes([0xFF; 4])?.is_empty());
    assert_eq!(VoiceCode::from_bytes(*b"AB\xFF\0")?.as_str(), "AB");
    Ok(())
}

#[test]
fn a_voice_code_edit_rewrites_only_its_page() -> TestResult {
    let captured = image(RepeaterListKind::Repeater, &[], NO_ENDS)?;
    let mut names = RepeaterNames::read(&captured)?;
    names.set_country_voice(CountryIndex::new(0)?, VoiceCode::new("USA")?);
    names.set_group_voice(GroupIndex::new(299)?, VoiceCode::new("SMP")?);
    let replacements = names.page_replacements(&captured)?;
    let pages: Vec<u32> = replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x3600, 0x3E00]);
    let reread = RepeaterNames::read(&applied(&captured, &replacements)?)?;
    assert_eq!(reread, names);
    assert_eq!(reread.group_voice(GroupIndex::new(299)?).as_str(), "SMP");
    Ok(())
}

#[test]
fn renamed_table_entries_rewrite_only_their_pages() -> TestResult {
    let captured = image(RepeaterListKind::Repeater, &[], NO_ENDS)?;
    let mut names = RepeaterNames::read(&captured)?;
    names.set_region(RegionIndex::new(0)?, StoredText::new("Asia")?);
    names.set_group(GroupIndex::new(299)?, StoredText::new("Last group")?);
    let replacements = names.page_replacements(&captured)?;
    let pages: Vec<u32> = replacements
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0x4_A000, 0x4_BD00]);
    let reread = RepeaterNames::read(&applied(&captured, &replacements)?)?;
    assert_eq!(reread, names);
    assert_eq!(
        reread.group(GroupIndex::new(299)?).to_string(),
        "Last group"
    );
    assert!(
        RepeaterNames::read(&captured)?
            .page_replacements(&captured)?
            .is_empty()
    );
    Ok(())
}
