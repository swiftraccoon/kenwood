//! Canonical coverage, exact identity matching, and deterministic sparse changes.

use std::cell::Cell;

use super::*;
use crate::types::{FirmwareIdentity, RadioModel, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type OwnedPages = Vec<(Page, Vec<u8>)>;

fn identity(firmware: &str, radio_type: &str) -> Result<Identity, ValidationError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new(firmware)?,
        radio_type: RadioType::new(radio_type)?,
    })
}

fn pages(fill: u8) -> OwnedPages {
    menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .map(|page| (page, vec![fill; page.len()]))
        .collect()
}

fn configuration<'a>(
    identity: &'a Identity,
    pages: &'a [(Page, Vec<u8>)],
) -> Result<StandardConfiguration<'a>, ConfigurationError> {
    StandardConfiguration::new(
        identity,
        pages.iter().map(|(page, bytes)| (*page, bytes.as_slice())),
    )
}

#[test]
fn identical_standard_configurations_compare_every_covered_byte() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let before = pages(0x5A);
    let after = before.clone();
    let before = configuration(&identity, &before)?;
    let after = configuration(&identity, &after)?;
    assert_eq!(
        before.identity(),
        &identity,
        "retain every identity component"
    );
    let diff = StandardConfigurationDiff::between(&before, &after)?;
    assert_eq!(
        diff.compared_pages(),
        1_138,
        "pin the standard transfer page count"
    );
    assert_eq!(
        diff.compared_bytes(),
        289_962,
        "count captured bytes, never dense gaps"
    );
    assert_eq!(
        diff.changed_bytes(),
        0,
        "identical input has no changed bytes"
    );
    assert!(
        diff.pages().is_empty(),
        "unchanged pages must not be stored"
    );
    Ok(())
}

fn set_byte(pages: &mut OwnedPages, address: u32, value: u8) -> TestResult {
    let (page, bytes) = pages
        .iter_mut()
        .find(|(page, _)| page.address().as_u32() <= address && address < page.end())
        .ok_or("test address is not captured")?;
    let offset = usize::try_from(address - page.address().as_u32())?;
    *bytes.get_mut(offset).ok_or("test byte missing")? = value;
    Ok(())
}

#[test]
fn first_last_sparse_boundaries_and_multiple_changes_keep_canonical_order() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let before = pages(0x11);
    let mut after = before.clone();
    let expected = [8, 47, 56, 327_681, 327_935, 376_319];
    for address in expected.into_iter().rev() {
        set_byte(&mut after, address, 0x22)?;
    }
    let before = configuration(&identity, &before)?;
    let after = configuration(&identity, &after)?;
    let diff = StandardConfigurationDiff::between(&before, &after)?;
    assert_eq!(
        diff.compared_pages(),
        1_138,
        "unchanged pages remain part of comparison"
    );
    assert_eq!(diff.compared_bytes(), 289_962, "gaps remain excluded");
    assert_eq!(
        diff.changed_bytes(),
        expected.len(),
        "count individual changed bytes"
    );
    assert_eq!(
        diff.pages().len(),
        4,
        "store only the four changed canonical pages"
    );
    let changes = diff
        .pages()
        .iter()
        .flat_map(ChangedPage::changes)
        .collect::<Vec<_>>();
    assert_eq!(
        changes
            .iter()
            .map(|change| change.address().as_u32())
            .collect::<Vec<_>>(),
        expected,
        "changes must follow canonical address order, not edit order"
    );
    for page in diff.pages() {
        assert!(
            !page.changes().is_empty(),
            "a retained changed page cannot be empty"
        );
        for change in page.changes() {
            assert!(
                page.page().region().contains(change.address()),
                "every change belongs to its reported page"
            );
            assert_eq!(change.before(), 0x11, "retain the exact original byte");
            assert_eq!(change.after(), 0x22, "retain the exact later byte");
        }
    }
    assert_eq!(
        diff,
        StandardConfigurationDiff::between(&before, &after)?,
        "repeated comparison must be deterministic"
    );
    Ok(())
}

#[test]
fn every_byte_may_change_without_including_uncovered_addresses() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let before = pages(0);
    let after = pages(255);
    let before = configuration(&identity, &before)?;
    let after = configuration(&identity, &after)?;
    let diff = StandardConfigurationDiff::between(&before, &after)?;
    assert_eq!(diff.pages().len(), 1_138, "all standard pages changed");
    assert_eq!(
        diff.changed_bytes(),
        289_962,
        "all and only captured bytes changed"
    );
    for changed in diff.pages() {
        assert_eq!(
            changed.changes().len(),
            changed.page().len(),
            "all bytes of each page changed"
        );
        assert_eq!(
            changed.changes().first().map(|byte| byte.address()),
            Some(changed.page().address()),
            "include each page's first byte"
        );
        assert_eq!(
            changed.changes().last().map(|byte| byte.address().as_u32()),
            Some(changed.page().end() - 1),
            "include each page's last byte"
        );
    }
    Ok(())
}

#[test]
fn comparison_direction_preserves_before_and_after_values() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let before = pages(0);
    let mut after = before.clone();
    set_byte(&mut after, 56, 0xFF)?;
    let before = configuration(&identity, &before)?;
    let after = configuration(&identity, &after)?;
    let forward = StandardConfigurationDiff::between(&before, &after)?;
    let reverse = StandardConfigurationDiff::between(&after, &before)?;
    let forward = forward
        .pages()
        .first()
        .and_then(|page| page.changes().first())
        .ok_or("forward change missing")?;
    let reverse = reverse
        .pages()
        .first()
        .and_then(|page| page.changes().first())
        .ok_or("reverse change missing")?;
    assert_eq!(
        forward.address(),
        reverse.address(),
        "comparison direction cannot move an address"
    );
    assert_eq!(
        forward.before(),
        reverse.after(),
        "reverse comparison must swap the old value"
    );
    assert_eq!(
        forward.after(),
        reverse.before(),
        "reverse comparison must swap the new value"
    );
    Ok(())
}

#[test]
fn missing_first_middle_or_last_page_is_rejected() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    for index in [0, 500, 1_137] {
        let mut input = pages(0);
        let (expected, _) = input.remove(index);
        let result = configuration(&identity, &input);
        if index == 1_137 {
            assert!(
                matches!(result, Err(ConfigurationError::MissingPage { index: 1_137, expected: actual }) if actual == expected),
                "missing final page must be rejected: {result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(ConfigurationError::UnexpectedPage { index: actual_index, expected: actual, .. }) if actual_index == index && actual == expected),
                "missing interior page must fail at the first changed position: {result:?}"
            );
        }
    }
    let result = StandardConfiguration::new(&identity, std::iter::empty());
    assert!(
        matches!(
            result,
            Err(ConfigurationError::MissingPage { index: 0, .. })
        ),
        "empty input cannot become a complete configuration: {result:?}"
    );
    Ok(())
}

#[test]
fn extra_or_duplicate_pages_are_never_silently_ignored() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let mut input = pages(0);
    let duplicate = input.first().ok_or("first page missing")?.clone();
    input.push(duplicate.clone());
    let result = configuration(&identity, &input);
    assert!(
        matches!(result, Err(ConfigurationError::ExtraPage { index: 1_138, actual }) if actual == duplicate.0),
        "trailing duplicates must fail: {result:?}"
    );
    let mut input = pages(0);
    input.insert(1, duplicate);
    let result = configuration(&identity, &input);
    assert!(
        matches!(
            result,
            Err(ConfigurationError::UnexpectedPage { index: 1, .. })
        ),
        "interior duplicates must fail before later data: {result:?}"
    );
    Ok(())
}

#[test]
fn reordered_or_substituted_pages_are_rejected_at_their_first_position() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let mut input = pages(0);
    input.swap(200, 201);
    let result = configuration(&identity, &input);
    assert!(
        matches!(
            result,
            Err(ConfigurationError::UnexpectedPage { index: 200, .. })
        ),
        "page reordering must not be repaired silently: {result:?}"
    );
    let mut input = pages(0);
    let (page, bytes) = input.first_mut().ok_or("first page missing")?;
    *page = Page::new(Address::new(9)?, bytes.len())?;
    let result = configuration(&identity, &input);
    assert!(
        matches!(
            result,
            Err(ConfigurationError::UnexpectedPage { index: 0, .. })
        ),
        "a same-length page at a wrong address must fail: {result:?}"
    );
    Ok(())
}

#[test]
fn wrong_declared_page_length_or_payload_length_is_rejected() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let mut input = pages(0);
    let (page, _) = input.first_mut().ok_or("first page missing")?;
    *page = Page::new(page.address(), page.len() - 1)?;
    let result = configuration(&identity, &input);
    assert!(
        matches!(
            result,
            Err(ConfigurationError::UnexpectedPage { index: 0, .. })
        ),
        "declared lengths must match the schedule: {result:?}"
    );
    for length in [0, 39, 41, 257] {
        let mut input = pages(0);
        input
            .first_mut()
            .ok_or("first page missing")?
            .1
            .resize(length, 0);
        let result = configuration(&identity, &input);
        assert!(
            matches!(result, Err(ConfigurationError::PageLength { index: 0, actual, .. }) if actual == length),
            "truncated or oversized payloads must fail: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn startup_bitmap_and_gap_pages_cannot_extend_standard_coverage() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    for address in [48, 393_216] {
        let mut input = pages(0);
        let page = Page::new(Address::new(address)?, 8)?;
        input.push((page, vec![0; 8]));
        let result = configuration(&identity, &input);
        assert!(
            matches!(result, Err(ConfigurationError::ExtraPage { index: 1_138, actual }) if actual == page),
            "uncovered addresses are not part of standard comparison: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn exact_firmware_and_type_must_match_even_when_all_bytes_are_equal() -> TestResult {
    let before_identity = identity("1.02", "K,2,1")?;
    let input = pages(0);
    let before = configuration(&before_identity, &input)?;
    for after_identity in [
        identity("1.03", "K,2,1")?,
        identity("01.02", "K,2,1")?,
        identity("1.02", "J,2,1")?,
        identity("1.02", "K,2,2")?,
    ] {
        let after = configuration(&after_identity, &input)?;
        let result = StandardConfigurationDiff::between(&before, &after);
        assert!(
            matches!(result, Err(ConfigurationError::IdentityMismatch { before: ref actual_before, after: ref actual_after }) if actual_before == &before_identity && actual_after == &after_identity),
            "byte equality cannot override identity mismatch: {result:?}"
        );
    }
    // Other model identities cannot be constructed in this crate's typed model.
    assert!(
        RadioModel::try_from("OTHER").is_err(),
        "unsupported model identities remain unrepresentable"
    );
    Ok(())
}

#[test]
fn comparison_does_not_apply_a_firmware_schema_admission_gate() -> TestResult {
    let identity = identity("9.99", "UNQUALIFIED")?;
    let input = pages(0xA5);
    let before = configuration(&identity, &input)?;
    let after = configuration(&identity, &input)?;
    let diff = StandardConfigurationDiff::between(&before, &after)?;
    assert_eq!(
        diff.changed_bytes(),
        0,
        "byte comparison need not interpret a firmware schema"
    );
    assert_eq!(
        before.identity(),
        &identity,
        "retain unsupported provenance without changing it"
    );
    Ok(())
}

#[test]
fn infinite_trailing_input_is_rejected_after_one_extra_item() -> TestResult {
    let identity = identity("1.02", "K,2,1")?;
    let input = pages(0);
    let duplicate = input.first().ok_or("first page missing")?;
    let consumed = Cell::new(0);
    let input = input
        .iter()
        .map(|(page, bytes)| (*page, bytes.as_slice()))
        .chain(std::iter::repeat((duplicate.0, duplicate.1.as_slice())))
        .inspect(|_| consumed.set(consumed.get() + 1));
    let result = StandardConfiguration::new(&identity, input);
    assert!(
        matches!(
            result,
            Err(ConfigurationError::ExtraPage { index: 1_138, .. })
        ),
        "infinite excess input must fail promptly: {result:?}"
    );
    assert_eq!(
        consumed.get(),
        1_139,
        "consume at most the schedule and first excess item"
    );
    Ok(())
}
