use super::*;
use crate::types::{FirmwareIdentity, RadioModel, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity() -> Result<Identity, ValidationError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn page(address: u32, length: usize) -> Result<Page, ValidationError> {
    Page::new(Address::new(address)?, length)
}

/// Every page of the standard schedule, each filled with `fill`.
fn standard(fill: u8) -> Result<Configuration, Box<dyn std::error::Error>> {
    let pages = menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .map(|page| (page, vec![fill; page.len()]));
    Ok(Configuration::from_pages(identity()?, pages)?)
}

#[test]
fn coverage_is_merged_and_checked_before_bytes_are_returned() -> TestResult {
    let configuration = Configuration::from_pages(
        identity()?,
        [
            (page(0x1_0100, 256)?, vec![2; 256]),
            (page(0x1_0000, 256)?, vec![1; 256]),
            (page(0x2_0000, 16)?, vec![3; 16]),
        ],
    )?;
    assert_eq!(
        configuration.coverage(),
        [
            Region::new(0x1_0000, 0x1_0200)?,
            Region::new(0x2_0000, 0x2_0010)?
        ],
        "adjacent pages merge; the order is by address"
    );
    assert_eq!(
        configuration.bytes(Region::new(0x1_00FF, 0x1_0101)?)?,
        [1, 2],
        "a range across adjacent pages is covered"
    );
    for uncovered in [
        Region::new(0x1_01FF, 0x1_0201)?,
        Region::new(0x3_0000, 0x3_0001)?,
    ] {
        let result = configuration.bytes(uncovered);
        assert!(
            matches!(result, Err(ConfigurationError::Uncovered { .. })),
            "{uncovered:?}: {result:?}"
        );
    }
    assert!(
        configuration
            .image_covering(Region::new(0x2_0000, 0x2_0010)?)
            .is_ok()
    );
    assert!(!configuration.is_standard());
    assert!(configuration.standard().is_err());
    Ok(())
}

#[test]
fn overlapping_or_misshapen_pages_are_refused() -> TestResult {
    let overlap = Configuration::from_pages(
        identity()?,
        [
            (page(0x1_0000, 256)?, vec![0; 256]),
            (page(0x1_0080, 16)?, vec![0; 16]),
        ],
    );
    assert!(
        matches!(overlap, Err(ConfigurationError::PageOverlap { .. })),
        "{overlap:?}"
    );
    let short = Configuration::from_pages(identity()?, [(page(0x1_0000, 256)?, vec![0; 255])]);
    assert!(
        matches!(
            short,
            Err(ConfigurationError::PageLength { actual: 255, .. })
        ),
        "{short:?}"
    );
    Ok(())
}

#[test]
fn the_persisted_form_round_trips_and_detects_damage() -> TestResult {
    let configuration = Configuration::from_pages(
        identity()?,
        [
            (page(8, 40)?, vec![0x11; 40]),
            (page(0x1_0000, 256)?, (0..=255).collect()),
        ],
    )?;
    let stored = configuration.to_bytes();
    assert_eq!(stored.get(..8), Some(&b"TMD750CF"[..]));
    assert_eq!(Configuration::from_bytes(&stored)?, configuration);

    let mut flipped = stored.clone();
    *flipped.get_mut(40).ok_or("a stored byte")? ^= 0x01;
    assert!(matches!(
        Configuration::from_bytes(&flipped),
        Err(ConfigurationError::Checksum { .. })
    ));
    let mut other_version = stored.clone();
    *other_version.get_mut(8).ok_or("the version byte")? = 2;
    assert!(Configuration::from_bytes(&other_version).is_err());
    for damaged in [
        stored.get(..stored.len() - 1).ok_or("truncated")?.to_vec(),
        b"NOTACONF".to_vec(),
        Vec::new(),
    ] {
        let result = Configuration::from_bytes(&damaged);
        assert!(
            matches!(
                result,
                Err(ConfigurationError::Format { .. } | ConfigurationError::Checksum { .. })
            ),
            "{} bytes: {result:?}",
            damaged.len()
        );
    }
    Ok(())
}

#[test]
fn a_standard_capture_offers_the_standard_views_and_diff() -> TestResult {
    let before = standard(0xA5)?;
    assert!(before.is_standard());
    assert_eq!(
        before.standard()?.identity(),
        &identity()?,
        "the standard view keeps the identity"
    );
    let _menu = before.menu_snapshot()?;
    let unchanged = before.diff(&standard(0xA5)?)?;
    assert_eq!(unchanged.changed_bytes(), 0);
    assert_eq!(unchanged.compared_bytes(), 289_962);
    let changed = before.diff(&standard(0x5A)?)?;
    assert_eq!(changed.changed_bytes(), 289_962);
    let round_trip = Configuration::from_bytes(&before.to_bytes())?;
    assert_eq!(round_trip.diff(&before)?.changed_bytes(), 0);
    Ok(())
}

/// `stored` with the first page's address replaced and the CRC recomputed.
fn with_first_address(stored: &[u8], address: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Magic, version, the three identity strings (TM-D750, 1.02, K,2,1) and
    // the page count precede the first page's address.
    const FIRST_ADDRESS: usize = 8 + 1 + (1 + 7) + (1 + 4) + (1 + 5) + 4;
    let (body, _crc) = stored.split_last_chunk::<4>().ok_or("a stored form")?;
    let mut body = body.to_vec();
    body.get_mut(FIRST_ADDRESS..FIRST_ADDRESS + 4)
        .ok_or("the first address")?
        .copy_from_slice(&address.to_le_bytes());
    let checksum = crc32(&body);
    body.extend_from_slice(&checksum.to_le_bytes());
    Ok(body)
}

#[test]
fn a_stored_page_outside_the_image_is_named_as_such() -> TestResult {
    let configuration =
        Configuration::from_pages(identity()?, [(page(0x1_0000, 256)?, vec![0x22; 256])])?;
    let stored = configuration.to_bytes();
    assert_eq!(
        Configuration::from_bytes(&with_first_address(&stored, 0x1_0000)?)?,
        configuration,
        "the crafted form matches the original at the original address"
    );
    let image_length = u32::try_from(crate::types::IMAGE_LENGTH)?;
    for address in [image_length, image_length - 16] {
        let result = Configuration::from_bytes(&with_first_address(&stored, address)?);
        assert!(
            matches!(
                result,
                Err(ConfigurationError::Format {
                    detail: "a page lies outside the image"
                })
            ),
            "{address:#X}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn the_checksum_is_the_ieee_crc32() {
    assert_eq!(
        crc32(b"123456789"),
        0xCBF4_3926,
        "the published check value"
    );
    assert_eq!(crc32(b""), 0);
}
