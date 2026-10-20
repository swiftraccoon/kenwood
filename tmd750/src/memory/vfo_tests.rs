use std::collections::BTreeSet;

use super::*;
use crate::types::IMAGE_LENGTH;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Band A's current VFO record in a firmware 1.02 backup.
const CURRENT_A_BYTES: [u8; 40] = [
    0x70, 0x6C, 0xA7, 0x08, 0xC0, 0x27, 0x09, 0x00, 0x22, 0x01, 0x02, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// Band B's current VFO record in the same backup.
const CURRENT_B_BYTES: [u8; 40] = [
    0xE0, 0x03, 0x4B, 0x0D, 0x00, 0x6A, 0x18, 0x00, 0x88, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// The A 430 MHz VFO band record in the same backup: 446.000 MHz, simplex,
/// a 5 MHz offset and 25 kHz steps.
const A430_BYTES: [u8; 40] = [
    0x80, 0x6B, 0x95, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// The B 430 MHz VFO band record in the same backup: the A 430 MHz record
/// at 440.000 MHz.
const B430_BYTES: [u8; 40] = [
    0x00, 0xDE, 0x39, 0x1A, 0x40, 0x4B, 0x4C, 0x00, 0x99, 0x01, 0x00, 0x08, 0x08, 0x00, 0x00, 0x43,
    0x51, 0x43, 0x51, 0x43, 0x51, 0x00, 0x00, 0x44, 0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x44,
    0x49, 0x52, 0x45, 0x43, 0x54, 0x00, 0x00, 0x00,
];

/// The band B step segment table in the same backup: a step and a mode byte
/// for each of the seventeen segments.
const SEGMENT_BYTES: [u8; 34] = [
    0x06, 0x02, 0x02, 0x00, 0x02, 0x00, 0x02, 0x00, 0x0B, 0x00, 0x06, 0x00, 0x08, 0x00, 0x06, 0x00,
    0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x06, 0x00, 0x09, 0x00, 0x06, 0x00,
    0x0B, 0x00,
];

/// The `FO` record each band reported while those records were stored.
const FO_A: &str = "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
const FO_B: &str = "0223020000,0001600000,8,8,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00";

/// Offsets of the ten VFO band records from `0x50200` in that backup.
const OFFSETS: [usize; 10] = [0, 48, 96, 144, 256, 304, 352, 400, 448, 512];

/// Receive frequency of each VFO band record in that backup.
const BAND_HZ: [u32; 10] = [
    118_000_000,
    145_190_000,
    223_000_000,
    446_000_000,
    118_000_000,
    144_000_000,
    180_750_000,
    223_020_000,
    440_000_000,
    523_000_000,
];

/// The stored limit pairs, in megahertz: lower bound and end.
const LIMITS_MHZ: [(u32, u32); 10] = [
    (108, 136),
    (136, 174),
    (216, 260),
    (410, 470),
    (108, 136),
    (136, 174),
    (174, 216),
    (216, 410),
    (410, 470),
    (470, 524),
];

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

/// A record with the current band A bytes and `hz` as its receive frequency.
fn record_at(hz: u32) -> [u8; 40] {
    with_frequency(CURRENT_A_BYTES, hz)
}

/// A full image of `0x42` holding the bench VFO memory in `slot`.
fn bench_image(slot: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut image = vec![0x42; IMAGE_LENGTH];
    let base = 8192 * slot;
    put(&mut image, base + 0x5_0100, &CURRENT_A_BYTES)?;
    put(&mut image, base + 0x5_0128, &CURRENT_B_BYTES)?;
    put(&mut image, base + 0x5_0154, &[1])?;
    put(&mut image, base + 0x5_0164, &[7])?;
    for (index, (offset, hz)) in OFFSETS.into_iter().zip(BAND_HZ).enumerate() {
        let record = match index {
            1 => CURRENT_A_BYTES,
            3 => A430_BYTES,
            7 => CURRENT_B_BYTES,
            8 => B430_BYTES,
            _ => record_at(hz),
        };
        put(&mut image, base + 0x5_0200 + offset, &record)?;
        put(&mut image, base + 0x5_0200 + offset + 40, &[0xFF; 8])?;
    }
    for (index, (lower, end)) in LIMITS_MHZ.into_iter().enumerate() {
        let address = base + 0x5_0488 + 8 * index;
        put(&mut image, address, &(lower * 1_000_000).to_le_bytes())?;
        put(&mut image, address + 4, &(end * 1_000_000).to_le_bytes())?;
    }
    put(&mut image, base + 0x5_04DA, &SEGMENT_BYTES)?;
    put(&mut image, base + 0x5_0513, &[1])?;
    Ok(image)
}

/// `bytes` with `hz` as the receive frequency.
fn with_frequency(mut bytes: [u8; 40], hz: u32) -> [u8; 40] {
    for (destination, source) in bytes.iter_mut().zip(hz.to_le_bytes()) {
        *destination = source;
    }
    bytes
}

fn slot(index: u8) -> Result<SlotIndex, Box<dyn std::error::Error>> {
    Ok(SlotIndex::new(index)?)
}

#[test]
fn vfo_bands_follow_the_stored_layout() -> TestResult {
    for (index, band) in (0_u8..).zip(VfoBand::ALL) {
        assert_eq!(band.index(), index);
        assert_eq!(VfoBand::from_index(index), Some(band));
        let offset = OFFSETS.get(usize::from(index)).ok_or("an offset")?;
        assert_eq!(band.record_address(slot(0)?) as usize, 0x5_0200 + offset);
        assert_eq!(
            band.limits_address(slot(0)?) as usize,
            0x5_0488 + 8 * usize::from(index)
        );
        assert_eq!(
            band.record_address(slot(1)?),
            band.record_address(slot(0)?) + 8192
        );
        assert_eq!(band.band(), if index < 4 { Band::A } else { Band::B });
    }
    assert_eq!(VfoBand::from_index(10), None);
    assert_eq!(VfoBand::B200And300.to_string(), "B 200 & 300 MHz");
    Ok(())
}

#[test]
fn slot_zero_decodes_to_what_fo_reported() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let memory = VfoMemory::read(&image, slot(0)?)?;
    assert_eq!(memory.current(Band::A).cat_record()?.to_wire_fields(), FO_A);
    assert_eq!(memory.current(Band::B).cat_record()?.to_wire_fields(), FO_B);
    assert_eq!(memory.selected(Band::A), VfoBand::A144);
    assert_eq!(memory.selected(Band::B), VfoBand::B200And300);
    assert_eq!(memory.band(VfoBand::A144).record(), memory.current(Band::A));
    assert_eq!(
        memory.band(VfoBand::B200And300).record(),
        memory.current(Band::B)
    );
    for ((band, hz), (lower, end)) in VfoBand::ALL.into_iter().zip(BAND_HZ).zip(LIMITS_MHZ) {
        let stored = memory.band(band);
        assert_eq!(stored.band(), band);
        assert_eq!(stored.record().receive_frequency.as_hz(), hz);
        let limits = stored.limits();
        assert_eq!(
            (
                u32::from(limits.lower_mhz()),
                u32::from(limits.upper_mhz()) + 1
            ),
            (lower, end)
        );
    }
    assert!(memory.page_replacements(&image)?.is_empty());
    Ok(())
}

/// Image addresses whose bytes the replacements change.
fn changed(replacements: &[PageReplacement]) -> BTreeSet<usize> {
    replacements
        .iter()
        .flat_map(|replacement| {
            let start = replacement.page().address().as_usize();
            replacement
                .expected()
                .iter()
                .zip(replacement.replacement())
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .map(move |(offset, _)| start + offset)
        })
        .collect()
}

#[test]
fn tuning_selects_the_vfo_band_and_writes_only_its_records() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    let mut record = memory.current(Band::A).clone();
    record.receive_frequency = Frequency::new(446_100_000)?;
    assert_eq!(memory.tune(Band::A, record.clone())?, VfoBand::A430);
    assert_eq!(memory.selected(Band::A), VfoBand::A430);
    assert_eq!(memory.current(Band::A), &record);
    assert_eq!(memory.band(VfoBand::A430).record(), &record);
    let addresses = changed(&memory.page_replacements(&image)?);
    assert!(!addresses.is_empty());
    let allowed = |address: &usize| {
        (0x5_0100..0x5_0128).contains(address)
            || *address == 0x5_0154
            || (0x5_0290..0x5_02B8).contains(address)
    };
    assert!(addresses.iter().all(allowed), "{addresses:X?}");
    let mut beyond = record;
    beyond.receive_frequency = Frequency::new(523_000_000)?;
    assert_eq!(
        memory.tune(Band::A, beyond),
        Err(VfoError::OutsideLimits {
            band: Band::A,
            hz: 523_000_000
        })
    );
    Ok(())
}

#[test]
fn a_band_record_edit_keeps_the_current_record_in_step() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    let mut unselected = memory.band(VfoBand::B430).record().clone();
    unselected.receive_frequency = Frequency::new(441_000_000)?;
    memory.set_band_record(VfoBand::B430, unselected)?;
    assert_eq!(
        memory.current(Band::B).receive_frequency.as_hz(),
        223_020_000
    );
    let mut selected = memory.current(Band::B).clone();
    selected.receive_frequency = Frequency::new(223_040_000)?;
    memory.set_band_record(VfoBand::B200And300, selected.clone())?;
    assert_eq!(memory.current(Band::B), &selected);
    let mut outside = selected;
    outside.receive_frequency = Frequency::new(146_520_000)?;
    assert!(matches!(
        memory.set_band_record(VfoBand::B200And300, outside),
        Err(VfoError::OutsideLimits { band: Band::B, .. })
    ));
    Ok(())
}

#[test]
fn limits_stay_inside_their_band_and_around_stored_records() -> TestResult {
    assert!(VfoLimits::new(VfoBand::A144, 136, 173).is_ok());
    for (lower, upper) in [(135, 150), (140, 139), (140, 174)] {
        assert_eq!(
            VfoLimits::new(VfoBand::A144, lower, upper),
            Err(VfoError::InvalidLimits {
                band: VfoBand::A144,
                lower_mhz: lower,
                upper_mhz: upper
            })
        );
    }
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    assert_eq!(
        memory.set_limits(VfoBand::A144, 146, 150),
        Err(VfoError::LimitsExcludeRecord {
            band: VfoBand::A144
        })
    );
    memory.set_limits(VfoBand::A144, 144, 147)?;
    let limits = memory.band(VfoBand::A144).limits();
    assert!(limits.contains(Frequency::new(147_995_000)?));
    assert!(!limits.contains(Frequency::new(148_000_000)?));
    let replacements = memory.page_replacements(&image)?;
    let addresses = changed(&replacements);
    assert!(
        addresses
            .iter()
            .all(|address| (0x5_0490..0x5_0498).contains(address)),
        "{addresses:X?}"
    );
    let [replacement] = replacements.as_slice() else {
        return Err("one changed page".into());
    };
    let pair = replacement
        .replacement()
        .get(0x90..0x98)
        .ok_or("the limit pair")?;
    assert_eq!(pair.get(..4), Some(&144_000_000_u32.to_le_bytes()[..]));
    assert_eq!(pair.get(4..), Some(&148_000_000_u32.to_le_bytes()[..]));
    Ok(())
}

#[test]
fn malformed_selections_and_limits_are_refused() -> TestResult {
    let mut crossed = bench_image(0)?;
    put(&mut crossed, 0x5_0154, &[7])?;
    assert_eq!(
        VfoMemory::read(&MemoryImage::from_bytes(crossed)?, slot(0)?),
        Err(VfoError::Selection {
            band: Band::A,
            index: 7
        })
    );
    for (lower, end) in [
        (108_500_000_u32, 136_000_000_u32),
        (100_000_000, 136_000_000),
    ] {
        let mut odd = bench_image(0)?;
        put(&mut odd, 0x5_0488, &lower.to_le_bytes())?;
        put(&mut odd, 0x5_048C, &end.to_le_bytes())?;
        assert_eq!(
            VfoMemory::read(&MemoryImage::from_bytes(odd)?, slot(0)?),
            Err(VfoError::MalformedLimits {
                band: VfoBand::A118,
                lower,
                end
            })
        );
    }
    Ok(())
}

#[test]
fn another_slot_reads_at_its_stride() -> TestResult {
    let mut bytes = bench_image(2)?;
    put(&mut bytes, 2 * 8192 + 0x5_0100, &record_at(146_520_000))?;
    let image = MemoryImage::from_bytes(bytes)?;
    let memory = VfoMemory::read(&image, slot(2)?)?;
    assert_eq!(memory.slot(), slot(2)?);
    assert_eq!(
        memory.current(Band::A).receive_frequency.as_hz(),
        146_520_000
    );
    assert!(VfoMemory::read(&image, slot(0)?).is_err());
    Ok(())
}

fn frequency(hz: u32) -> Result<Frequency, Box<dyn std::error::Error>> {
    Ok(Frequency::new(hz)?)
}

#[test]
fn band_b_step_segments_follow_the_stored_table() -> TestResult {
    let ranges: Vec<(u16, u16)> = StepSegment::ALL.map(StepSegment::range_mhz).to_vec();
    assert_eq!(ranges.len(), 17);
    assert_eq!(ranges.first(), Some(&(108, 136)));
    assert_eq!(ranges.get(7..13), Some(&[(225, 410); 6][..]));
    assert_eq!(ranges.last(), Some(&(470, 524)));
    let index = |hz| frequency(hz).map(|frequency| StepSegment::of(frequency).index());
    assert_eq!(index(223_020_000)?, 6);
    assert_eq!(index(300_000_000)?, 7);
    assert_eq!(index(Frequency::MAX_HZ)?, 16);
    assert_eq!(StepSegment::ALL[0].address(slot(0)?), 0x5_04DA);
    assert_eq!(StepSegment::ALL[16].address(slot(1)?), 0x5_04FA + 8192);
    let memory = VfoMemory::read(&MemoryImage::from_bytes(bench_image(0)?)?, slot(0)?)?;
    let steps: Vec<Option<StepSize>> = [0, 6, 7, 14]
        .map(|index| {
            StepSegment::ALL
                .get(index)
                .and_then(|segment| memory.segment_step(*segment))
        })
        .to_vec();
    assert_eq!(
        steps,
        [
            Some(StepSize::Hz12500),
            Some(StepSize::Hz20000),
            Some(StepSize::Hz12500),
            Some(StepSize::Hz25000)
        ]
    );
    assert!(memory.auto_repeater_offset());
    Ok(())
}

/// Each band's current record after `FQ` set the frequency on firmware 1.02,
/// read from the next standard backup, from the bench memory.
#[test]
fn frequency_tuning_reproduces_what_fq_stored() -> TestResult {
    let memory = VfoMemory::read(&MemoryImage::from_bytes(bench_image(0)?)?, slot(0)?)?;
    let mut segment_step = with_frequency(CURRENT_B_BYTES, 300_000_000);
    segment_step[8] = 0x68;
    let cases = [
        (Band::B, 300_000_000, VfoBand::B200And300, segment_step),
        (
            Band::B,
            446_000_000,
            VfoBand::B430,
            with_frequency(B430_BYTES, 446_000_000),
        ),
        (
            Band::A,
            445_000_000,
            VfoBand::A430,
            with_frequency(A430_BYTES, 445_000_000),
        ),
    ];
    for (band, hz, vfo_band, expected) in cases {
        let (selected, record) = memory.frequency_record(band, frequency(hz)?)?;
        assert_eq!(selected, vfo_band, "{band} {hz}");
        assert_eq!(record.to_bytes(), expected, "{band} {hz}");
    }
    for (hz, shift) in [
        (147_000_000, ShiftDirection::Plus),
        (146_520_000, ShiftDirection::Simplex),
        (145_190_000, ShiftDirection::Minus),
    ] {
        let (selected, record) = memory.frequency_record(Band::A, frequency(hz)?)?;
        assert_eq!(selected, VfoBand::A144);
        assert_eq!(record.shift, shift, "{hz}");
        assert_eq!(record.transmit, memory.current(Band::A).transmit);
        assert_eq!(record.tone_mode, memory.current(Band::A).tone_mode);
    }
    assert_eq!(
        memory.frequency_record(Band::A, frequency(180_000_000)?),
        Err(VfoError::OutsideLimits {
            band: Band::A,
            hz: 180_000_000
        })
    );
    Ok(())
}

#[test]
fn a_frequency_tune_writes_the_bytes_fq_changed() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    assert_eq!(
        memory.tune_frequency(Band::B, frequency(446_000_000)?)?,
        VfoBand::B430
    );
    assert_eq!(memory.selected(Band::B), VfoBand::B430);
    assert_eq!(memory.band(VfoBand::B430).record(), memory.current(Band::B));
    let radio: BTreeSet<usize> = (0x5_0128..=0x5_012E)
        .chain([0x5_0130, 0x5_0164])
        .chain(0x5_03C0..=0x5_03C2)
        .collect();
    assert_eq!(changed(&memory.page_replacements(&image)?), radio);
    Ok(())
}

#[test]
fn the_type_k_offset_plan_applies_only_where_the_radio_applies_it() -> TestResult {
    let shift = |hz| frequency(hz).map(type_k_shift);
    for (hz, expected) in [
        (145_099_999, ShiftDirection::Simplex),
        (145_100_000, ShiftDirection::Minus),
        (145_500_000, ShiftDirection::Simplex),
        (146_000_000, ShiftDirection::Plus),
        (146_400_000, ShiftDirection::Simplex),
        (146_600_000, ShiftDirection::Minus),
        (147_399_999, ShiftDirection::Plus),
        (147_600_000, ShiftDirection::Minus),
        (148_000_000, ShiftDirection::Simplex),
        (223_920_000, ShiftDirection::Minus),
        (225_000_000, ShiftDirection::Simplex),
        (442_000_000, ShiftDirection::Plus),
        (445_000_000, ShiftDirection::Simplex),
        (447_000_000, ShiftDirection::Minus),
        (450_000_000, ShiftDirection::Simplex),
    ] {
        assert_eq!(shift(hz)?, expected, "{hz}");
    }
    let mut bytes = bench_image(0)?;
    put(&mut bytes, 0x5_0513, &[0])?;
    let off = VfoMemory::read(&MemoryImage::from_bytes(bytes)?, slot(0)?)?;
    assert!(!off.auto_repeater_offset());
    let (_, kept) = off.frequency_record(Band::A, frequency(147_000_000)?)?;
    assert_eq!(kept.shift, ShiftDirection::Minus);

    let mut memory = VfoMemory::read(&MemoryImage::from_bytes(bench_image(0)?)?, slot(0)?)?;
    let mut reversed = memory.current(Band::A).clone();
    reversed.reverse = true;
    assert_eq!(memory.tune(Band::A, reversed)?, VfoBand::A144);
    let (_, kept) = memory.frequency_record(Band::A, frequency(147_000_000)?)?;
    assert_eq!(kept.shift, ShiftDirection::Minus);
    let mut digital = memory.current(Band::A).clone();
    digital.reverse = false;
    digital.mode = OperatingMode::Dv.try_into()?;
    assert_eq!(memory.tune(Band::A, digital)?, VfoBand::A144);
    let (_, kept) = memory.frequency_record(Band::A, frequency(147_000_000)?)?;
    assert_eq!(kept.shift, ShiftDirection::Minus);
    Ok(())
}

#[test]
fn frequency_entry_rounds_down_to_the_receive_step() -> TestResult {
    let memory = VfoMemory::read(&MemoryImage::from_bytes(bench_image(0)?)?, slot(0)?)?;
    for (band, hz, expected) in [
        (Band::A, 146_521_000, 146_520_000),
        (Band::B, 300_010_000, 300_000_000),
        (Band::B, 223_030_000, 223_020_000),
    ] {
        let (_, record) = memory.frequency_record(band, frequency(hz)?)?;
        assert_eq!(record.receive_frequency.as_hz(), expected, "{band} {hz}");
    }
    Ok(())
}

/// The radio's own sequence on firmware 1.02: selecting 12.5 kHz at 223.020
/// MHz (rounding to 223.0125), then entering 300.000 MHz and 223.020 MHz,
/// tuned 223.0125 MHz with the 12.5 kHz step.
#[test]
fn a_band_b_step_is_remembered_for_its_segment() -> TestResult {
    let image = MemoryImage::from_bytes(bench_image(0)?)?;
    let mut memory = VfoMemory::read(&image, slot(0)?)?;
    let mut selected = memory.current(Band::B).clone();
    selected.receive_frequency = frequency(223_012_500)?;
    selected.receive_step = StepSize::Hz12500;
    assert_eq!(memory.tune(Band::B, selected)?, VfoBand::B200And300);
    assert_eq!(
        memory.segment_step(StepSegment::of(frequency(223_020_000)?)),
        Some(StepSize::Hz12500)
    );
    assert!(changed(&memory.page_replacements(&image)?).contains(&0x5_04E6));
    assert_eq!(
        memory.tune_frequency(Band::B, frequency(300_000_000)?)?,
        VfoBand::B200And300
    );
    assert_eq!(
        memory.tune_frequency(Band::B, frequency(223_020_000)?)?,
        VfoBand::B200And300
    );
    let record = memory.current(Band::B);
    assert_eq!(record.receive_frequency.as_hz(), 223_012_500);
    assert_eq!(record.receive_step, StepSize::Hz12500);

    let mut band_a = memory.current(Band::A).clone();
    band_a.receive_step = StepSize::Hz25000;
    assert_eq!(memory.tune(Band::A, band_a)?, VfoBand::A144);
    assert_eq!(
        memory.segment_step(StepSegment::of(frequency(145_190_000)?)),
        Some(StepSize::Hz5000),
        "band A keeps no segment steps"
    );
    Ok(())
}
