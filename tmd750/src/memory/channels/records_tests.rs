use super::*;
use crate::protocol::channel::parse_memory_channel;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A 223.500 MHz DV record with scan lockout, as a CAT `ME` write carries it.
const STORED: &str = "996,0223500000,0001600000,8,8,1,0,0,1,0,0,0,2,20,20,050,0,W1AW/XYZ,2,07,1";

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

/// A full image of `0x42` whose channel tables hold every regular, program
/// scan and priority slot empty as firmware 1.02 stores it: band `FF`, no
/// lockout, the slot's default group, byte 3 `FF`, the empty record and an
/// unnamed name.
fn empty_tables() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    for index in 0..=1100_u16 {
        let channel = PhysicalChannel::new(index)?;
        let group = match index {
            0..=999 => u8::try_from(index / 100)?,
            1100 => 0x1F,
            _ => 0x1E,
        };
        put(
            &mut image,
            channel.flag_address(),
            &[0xFF, 0x00, group, 0xFF],
        )?;
        put(&mut image, channel.data_address(), &EMPTY_CHANNEL_RECORD)?;
        put(&mut image, channel.name_address(), &[0; CHANNEL_NAME_SIZE])?;
    }
    Ok(image)
}

/// `before` with every replacement written, each checked against the bytes
/// it expects.
fn apply(
    before: &[u8],
    replacements: &[PageReplacement],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut after = before.to_vec();
    for replacement in replacements {
        let start = replacement.page().address().as_usize();
        assert_eq!(
            before.get(start..start + replacement.expected().len()),
            Some(replacement.expected())
        );
        put(&mut after, start, replacement.replacement())?;
    }
    Ok(after)
}

fn page_addresses(plan: &MemoryChannelPlan) -> Vec<u32> {
    plan.pages()
        .iter()
        .map(|page| page.address().as_u32())
        .collect()
}

#[test]
fn a_store_writes_the_band_code_lockout_and_record_of_a_cat_store() -> TestResult {
    let mut before = empty_tables()?;
    let channel = PhysicalChannel::new(990)?;
    put(&mut before, channel.flag_address() + 2, &[12])?;
    put(
        &mut before,
        channel.name_address(),
        b"Simplex\0\0\0\0\0\0\0\0\0",
    )?;
    let (_, record) = parse_memory_channel(STORED)?;
    let plan = MemoryChannelPlan::new([(
        MemoryChannelAddress::regular(990)?,
        ChannelWrite::Store(record.clone()),
    )])?;
    assert_eq!(page_addresses(&plan), [0x2F00, 0xE500]);
    let after = apply(
        &before,
        &plan.page_replacements(&MemoryImage::from_bytes(before.clone())?)?,
    )?;
    let mut expected = before.clone();
    put(&mut expected, channel.flag_address(), &[0x02, 0x01])?;
    put(
        &mut expected,
        channel.data_address(),
        &StoredChannel::from_cat_record(&record)?.to_bytes(),
    )?;
    assert_eq!(after, expected);
    Ok(())
}

#[test]
fn the_priority_channel_is_stored_like_a_regular_channel() -> TestResult {
    let before = empty_tables()?;
    let (_, record) = parse_memory_channel(STORED)?;
    let plan = MemoryChannelPlan::new([(
        MemoryChannelAddress::Priority,
        ChannelWrite::Store(record.clone()),
    )])?;
    let after = apply(
        &before,
        &plan.page_replacements(&MemoryImage::from_bytes(before.clone())?)?,
    )?;
    let channel = PhysicalChannel::from_address(MemoryChannelAddress::Priority);
    let mut expected = before;
    put(&mut expected, channel.flag_address(), &[0x02, 0x01])?;
    put(
        &mut expected,
        channel.data_address(),
        &StoredChannel::from_cat_record(&record)?.to_bytes(),
    )?;
    assert_eq!(after, expected);
    assert_eq!(
        plan.writes().collect::<Vec<_>>(),
        [(MemoryChannelAddress::Priority, &ChannelWrite::Store(record))]
    );
    Ok(())
}

#[test]
fn a_clear_writes_the_slot_a_cat_clear_leaves() -> TestResult {
    let mut before = empty_tables()?;
    let channel = PhysicalChannel::new(995)?;
    let (_, record) = parse_memory_channel(STORED)?;
    put(
        &mut before,
        channel.flag_address(),
        &[0x02, 0x01, 0x09, 0x00],
    )?;
    put(
        &mut before,
        channel.data_address(),
        &StoredChannel::from_cat_record(&record)?.to_bytes(),
    )?;
    put(
        &mut before,
        channel.name_address(),
        b"Repeater\0\0\0\0\0\0\0\0",
    )?;
    let plan =
        MemoryChannelPlan::new([(MemoryChannelAddress::regular(995)?, ChannelWrite::Clear)])?;
    assert_eq!(page_addresses(&plan), [0x2F00, 0xE500, 0x1_3E00]);
    let after = apply(
        &before,
        &plan.page_replacements(&MemoryImage::from_bytes(before.clone())?)?,
    )?;
    let mut expected = before.clone();
    put(&mut expected, channel.flag_address(), &[0xFF, 0x00])?;
    put(&mut expected, channel.data_address(), &EMPTY_CHANNEL_RECORD)?;
    put(
        &mut expected,
        channel.name_address(),
        &[0; CHANNEL_NAME_SIZE],
    )?;
    assert_eq!(after, expected);

    let empty = MemoryImage::from_bytes(empty_tables()?)?;
    assert_eq!(plan.page_replacements(&empty)?, []);
    Ok(())
}

#[test]
fn a_plan_writes_each_channel_once() -> TestResult {
    let address = MemoryChannelAddress::program_scan_lower(5)?;
    assert_eq!(
        MemoryChannelPlan::new([
            (address, ChannelWrite::Clear),
            (address, ChannelWrite::Clear)
        ]),
        Err(ValidationError::DuplicateMemoryChannel { address })
    );
    Ok(())
}
