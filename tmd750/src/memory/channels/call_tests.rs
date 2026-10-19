use std::collections::BTreeSet;

use super::*;
use crate::types::{IMAGE_LENGTH, OffsetFrequency, TransmitSetting};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Bytes 15..40 of every CALL record read from firmware 1.02: `CQCQCQ`, two
/// `DIRECT` repeaters and digital code 0.
const TAIL: [u8; 25] = *b"CQCQCQ\0\0DIRECT\0\0DIRECT\0\0\0";

/// Flag and record bytes 0..15 read from firmware 1.02, in table order, with
/// the `FO` record CALL mode reported for each channel.
const STORED: [([u8; 4], [u8; 15], &str); 6] = [
    (
        [0x01, 0xFF, 0x22, 0xFF],
        [
            0xC0, 0xB7, 0xBB, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x01, 0x00, 0x08, 0x08, 0x00,
            0x00,
        ],
        "0146520000,0000600000,2,2,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
    ),
    (
        [0x01, 0xFF, 0x22, 0xFF],
        [
            0x00, 0x44, 0x95, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x11, 0x00, 0x08, 0x08, 0x00,
            0x00,
        ],
        "0144000000,0000600000,2,2,1,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
    ),
    (
        [0x02, 0xFF, 0x22, 0xFF],
        [
            0xE0, 0x56, 0x52, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x01, 0x00, 0x08, 0x08, 0x00,
            0x00,
        ],
        "0223500000,0001600000,8,8,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
    ),
    (
        [0x02, 0xFF, 0x22, 0xFF],
        [
            0xC0, 0xB5, 0x4A, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x11, 0x00, 0x08, 0x08, 0x00,
            0x00,
        ],
        "0223000000,0001600000,8,8,1,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
    ),
    (
        [0x03, 0xFF, 0x22, 0xFF],
        [
            0x80, 0x6B, 0x95, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x01, 0x00, 0x08, 0x08, 0x00,
            0x00,
        ],
        "0446000000,0005000000,9,9,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
    ),
    (
        [0x03, 0xFF, 0x22, 0xFF],
        [
            0x00, 0xDE, 0x39, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x11, 0x00, 0x08, 0x08, 0x00,
            0x00,
        ],
        "0440000000,0005000000,9,9,1,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00",
    ),
];

/// The names firmware 1.02 holds at name-table indexes 1136 to 1141.
const NAMES: [&str; 6] = [
    "Call VHF (FM)",
    "Call VHF (DV)",
    "Call 220M (FM)",
    "Call 220M (DV)",
    "Call UHF (FM)",
    "Call UHF (DV)",
];

fn record(head: [u8; 15]) -> [u8; CHANNEL_RECORD_SIZE] {
    let mut bytes = [0; CHANNEL_RECORD_SIZE];
    for (destination, source) in bytes.iter_mut().zip(head.into_iter().chain(TAIL)) {
        *destination = source;
    }
    bytes
}

fn name(text: &str) -> [u8; CHANNEL_NAME_SIZE] {
    let mut bytes = [0; CHANNEL_NAME_SIZE];
    for (destination, source) in bytes.iter_mut().zip(text.bytes()) {
        *destination = source;
    }
    bytes
}

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

/// Image addresses of the six flags, records and names in a firmware 1.02
/// backup, in table order.
const ADDRESSES: [(usize, usize, usize); 6] = [
    (0x31B0, 0xFCA0, 0x1_4700),
    (0x31B4, 0xFCC8, 0x1_4710),
    (0x31B8, 0xFD00, 0x1_4720),
    (0x31BC, 0xFD28, 0x1_4730),
    (0x31C0, 0xFD50, 0x1_4740),
    (0x31C4, 0xFD78, 0x1_4750),
];

/// A full image of `0xFF` holding the six stored CALL channels where the
/// backup held them.
fn stored_image() -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let mut image = vec![0xFF; IMAGE_LENGTH];
    for (((flag_at, data_at, name_at), (flag, head, _)), text) in
        ADDRESSES.into_iter().zip(STORED).zip(NAMES)
    {
        put(&mut image, flag_at, &flag)?;
        put(&mut image, data_at, &record(head))?;
        put(&mut image, name_at, &name(text))?;
    }
    Ok(MemoryImage::from_bytes(image)?)
}

fn entries(image: &MemoryImage) -> Result<Vec<CallChannelEntry>, ValidationError> {
    crate::memory::ChannelAccess::from_memory_image(image)
        .call_channels()
        .collect()
}

#[test]
fn call_records_follow_the_weather_slots_and_their_names_sit_four_slots_later() {
    let indexes: Vec<u16> = CallChannel::ALL
        .iter()
        .map(|channel| channel.index())
        .collect();
    assert_eq!(indexes, [1132, 1133, 1134, 1135, 1136, 1137]);
    let addresses: Vec<(usize, usize, usize)> = CallChannel::ALL
        .iter()
        .map(|channel| {
            (
                channel.flag_address(),
                channel.data_address(),
                channel.name_address(),
            )
        })
        .collect();
    assert_eq!(addresses, ADDRESSES);
    let first = CallChannel::new(CallBand::Vhf, CallMode::Fm);
    assert_eq!(first.to_string(), "CALL VHF FM");
    assert_eq!(
        CallChannel::new(CallBand::Band220, CallMode::Dv).to_string(),
        "CALL 220 MHz DV"
    );
}

#[test]
fn stored_call_records_decode_to_what_call_mode_reports() -> TestResult {
    let image = stored_image()?;
    let decoded = entries(&image)?;
    assert_eq!(decoded.len(), 6);
    for ((entry, (flag, head, reported)), text) in decoded.iter().zip(STORED).zip(NAMES) {
        let stored = entry.record().ok_or("a stored CALL record")?;
        assert_eq!(stored.cat_record()?.to_wire_fields(), reported);
        assert_eq!(stored.to_bytes(), record(head), "decoding is lossless");
        assert_eq!(stored.mode, entry.channel().mode().operating_mode());
        assert_eq!(entry.flag(), flag);
        assert_eq!(entry.name().text(), text);
    }
    assert_eq!(
        decoded.first().ok_or("CALL VHF FM")?.to_string(),
        "CALL VHF FM: 146.520000 MHz FM \"Call VHF (FM)\""
    );
    Ok(())
}

#[test]
fn an_empty_call_flag_leaves_the_record_undecoded() -> TestResult {
    let channel = CallChannel::new(CallBand::Band220, CallMode::Fm);
    let entry = CallChannelEntry::from_records(channel, &[0xFF; 4], &[0xFF; 40], &[0; 16])?;
    assert!(entry.record().is_none());
    assert_eq!(entry.to_string(), "CALL 220 MHz FM: empty");
    assert!(matches!(
        CallChannelEntry::from_records(channel, &[0x02, 0xFF, 0x22, 0xFF], &[0xFF; 40], &[0; 16]),
        Err(ValidationError::FrequencyOutOfRange { .. })
    ));
    assert!(matches!(
        CallChannelEntry::from_records(channel, &[0xFF; 3], &[0xFF; 40], &[0; 16]),
        Err(ValidationError::StoredChannelLength { actual: 3 })
    ));
    Ok(())
}

fn stored_record(
    image: &MemoryImage,
    channel: CallChannel,
) -> Result<StoredChannel, Box<dyn std::error::Error>> {
    let entry = crate::memory::ChannelAccess::from_memory_image(image).call_channel(channel)?;
    Ok(entry.record().ok_or("a stored CALL record")?.clone())
}

#[test]
fn a_call_plan_rewrites_only_its_record_on_the_page() -> TestResult {
    let image = stored_image()?;
    let channel = CallChannel::new(CallBand::Uhf, CallMode::Fm);
    let mut edited = stored_record(&image, channel)?;
    edited.receive_frequency = Frequency::new(446_100_000)?;
    edited.transmit = TransmitSetting::Offset(OffsetFrequency::new(0)?);
    let plan = CallChannelPlan::new([(channel, edited.clone())])?;
    assert_eq!(plan.pages(), [Page::new(Address::new(0xFD00)?, PAGE_SIZE)?]);
    let replacements = plan.page_replacements(&image)?;
    let [replacement] = replacements.as_slice() else {
        return Err(format!("expected one page, got {}", replacements.len()).into());
    };
    let start = 0xFD00;
    assert_eq!(
        replacement.expected(),
        image
            .as_bytes()
            .get(start..start + PAGE_SIZE)
            .ok_or("page")?
    );
    let record_start = channel.data_address() - start;
    let changed: BTreeSet<usize> = replacement
        .expected()
        .iter()
        .zip(replacement.replacement())
        .enumerate()
        .filter(|(_, (before, after))| before != after)
        .map(|(offset, _)| offset)
        .collect();
    assert!(!changed.is_empty());
    assert!(
        changed
            .iter()
            .all(|offset| (record_start..record_start + CHANNEL_RECORD_SIZE).contains(offset)),
        "only the planned record changes: {changed:?}"
    );
    assert_eq!(
        replacement
            .replacement()
            .get(record_start..record_start + CHANNEL_RECORD_SIZE)
            .ok_or("record")?,
        edited.to_bytes()
    );
    Ok(())
}

#[test]
fn records_on_two_pages_plan_two_writes_in_address_order() -> TestResult {
    let image = stored_image()?;
    let vhf = CallChannel::new(CallBand::Vhf, CallMode::Fm);
    let uhf = CallChannel::new(CallBand::Uhf, CallMode::Dv);
    let mut simplex = stored_record(&image, vhf)?;
    simplex.receive_frequency = Frequency::new(146_550_000)?;
    let mut digital = stored_record(&image, uhf)?;
    digital.receive_frequency = Frequency::new(441_000_000)?;
    let plan = CallChannelPlan::new([(uhf, digital), (vhf, simplex)])?;
    let pages: Vec<u32> = plan
        .page_replacements(&image)?
        .iter()
        .map(|replacement| replacement.page().address().as_u32())
        .collect();
    assert_eq!(pages, [0xFC00, 0xFD00]);
    let channels: Vec<CallChannel> = plan.records().map(|(channel, _)| channel).collect();
    assert_eq!(channels, [vhf, uhf]);
    Ok(())
}

#[test]
fn an_unchanged_record_plans_no_write() -> TestResult {
    let image = stored_image()?;
    let channel = CallChannel::new(CallBand::Band220, CallMode::Dv);
    let plan = CallChannelPlan::new([(channel, stored_record(&image, channel)?)])?;
    assert_eq!(plan.pages().len(), 1);
    assert!(plan.page_replacements(&image)?.is_empty());
    Ok(())
}

#[test]
fn a_call_record_keeps_its_band_range_and_mode() -> TestResult {
    let image = stored_image()?;
    let vhf = CallChannel::new(CallBand::Vhf, CallMode::Fm);
    let mut record = stored_record(&image, vhf)?;
    for (hz, admitted) in [
        (135_995_000, false),
        (136_000_000, true),
        (173_995_000, true),
        (174_000_000, false),
        (223_500_000, false),
    ] {
        record.receive_frequency = Frequency::new(hz)?;
        let result = CallChannelPlan::new([(vhf, record.clone())]);
        if admitted {
            assert!(result.is_ok(), "{hz}: {result:?}");
        } else {
            assert_eq!(
                result,
                Err(ValidationError::CallFrequencyOutOfRange {
                    channel: vhf,
                    hz,
                    minimum_hz: 136_000_000,
                    end_hz: 174_000_000,
                })
            );
        }
    }
    let dv = CallChannel::new(CallBand::Uhf, CallMode::Dv);
    let mut analog = stored_record(&image, dv)?;
    analog.mode = OperatingMode::Fm;
    assert_eq!(
        CallChannelPlan::new([(dv, analog)]),
        Err(ValidationError::CallChannelMode {
            channel: dv,
            expected: OperatingMode::Dv,
            actual: OperatingMode::Fm,
        })
    );
    let stored = stored_record(&image, dv)?;
    assert_eq!(
        CallChannelPlan::new([(dv, stored.clone()), (dv, stored)]),
        Err(ValidationError::DuplicateCallChannel { channel: dv })
    );
    Ok(())
}

#[test]
fn a_call_plan_refuses_a_slot_the_image_holds_empty() -> TestResult {
    let image = stored_image()?;
    let channel = CallChannel::new(CallBand::Band220, CallMode::Fm);
    let plan = CallChannelPlan::new([(channel, stored_record(&image, channel)?)])?;
    let mut bytes = image.as_bytes().to_vec();
    put(&mut bytes, channel.flag_address(), &[0xFF; 4])?;
    let emptied = MemoryImage::from_bytes(bytes)?;
    assert!(matches!(
        plan.page_replacements(&emptied),
        Err(Error::Validation(ValidationError::CallChannelNotStored { channel: refused }))
            if refused == channel
    ));
    Ok(())
}
