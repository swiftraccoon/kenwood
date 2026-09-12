//! Pure replacement validation and fail-closed batch admission.

use crate::Radio;
use crate::transport::MockTransport;
use crate::types::Address;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn complete_canonical_fragments_and_pages_are_owned_without_normalization() -> TestResult {
    for (address, length) in [
        (8, 40),
        (56, 200),
        (256, 160),
        (480, 32),
        (512, 256),
        (327_681, 255),
        (332_800, 128),
    ] {
        let page = Page::new(Address::new(address)?, length)?;
        let mut expected = vec![0xA5; length];
        let mut desired = vec![0x5A; length];
        let replacement = PageReplacement::new(page, &expected, &desired)?;
        expected.fill(0);
        desired.fill(0);
        assert_eq!(
            replacement.page(),
            page,
            "retain the exact canonical transfer unit"
        );
        assert_eq!(
            replacement.expected(),
            vec![0xA5; length],
            "own the immutable full before-image"
        );
        assert_eq!(
            replacement.replacement(),
            vec![0x5A; length],
            "own the immutable full replacement"
        );
        assert!(
            !replacement.is_noop(),
            "different page bytes require an intended write"
        );
        let noop = PageReplacement::new(page, &expected, &expected)?;
        assert!(
            noop.is_noop(),
            "equal images are valid read-only comparisons"
        );
    }
    Ok(())
}

#[test]
fn protected_noncanonical_and_incomplete_images_are_rejected() -> TestResult {
    for (address, length, protected) in [
        (0, 8, true),
        (48, 8, true),
        (393_216, 256, true),
        (512, 128, false),
        (513, 255, false),
    ] {
        let page = Page::new(Address::new(address)?, length)?;
        let result = PageReplacement::new(page, &vec![0; length], &vec![1; length]);
        assert!(
            matches!(&result, Err(McpError::PageNotWritable { .. })) && protected
                || matches!(&result, Err(McpError::NonCanonicalPage { .. })) && !protected,
            "reject the exact protection or canonical-page violation: {result:?}"
        );
    }
    let page = Page::new(Address::new(512)?, 256)?;
    for (expected_len, replacement_len) in
        [(0, 256), (255, 256), (256, 255), (257, 256), (256, 257)]
    {
        let result = PageReplacement::new(page, &vec![0; expected_len], &vec![1; replacement_len]);
        assert!(
            matches!(result, Err(McpError::ReplacementLength { address: 512, page_len: 256, expected_len: actual_expected, replacement_len: actual_replacement }) if actual_expected == expected_len && actual_replacement == replacement_len),
            "both full-image lengths must be validated"
        );
    }
    Ok(())
}

#[test]
fn duplicate_and_overlapping_ranges_are_rejected_independently_of_input_order() -> TestResult {
    let first = PageReplacement::new(Page::new(Address::new(512)?, 256)?, &[0; 256], &[1; 256])?;
    let second = PageReplacement::new(Page::new(Address::new(768)?, 256)?, &[0; 256], &[1; 256])?;
    assert!(
        validate_batch(&[second.clone(), first.clone()]).is_ok(),
        "validation must not require caller-sorted distinct pages"
    );
    let duplicates = validate_batch(&[second, first.clone(), first]);
    assert!(
        matches!(
            duplicates,
            Err(McpError::DuplicateReplacement { address: 512 })
        ),
        "duplicate detection must cover nonadjacent input entries"
    );
    let overlapping = [
        Page::new(Address::new(512)?, 256)?,
        Page::new(Address::new(640)?, 128)?,
    ];
    assert!(
        matches!(
            validate_disjoint(&overlapping),
            Err(McpError::OverlappingReplacements {
                first: 512,
                second: 640
            })
        ),
        "overlap validation must reject intersecting ranges independently of canonical shape"
    );
    Ok(())
}

#[tokio::test]
async fn every_batch_element_is_revalidated_before_any_page_traffic() -> TestResult {
    for mutation in 0..3 {
        let first = PageReplacement::new(Page::new(Address::new(8)?, 40)?, &[0; 40], &[1; 40])?;
        let mut invalid =
            PageReplacement::new(Page::new(Address::new(512)?, 256)?, &[0; 256], &[1; 256])?;
        match mutation {
            0 => {
                let _removed = invalid.expected.pop();
            }
            1 => invalid.page = Page::new(Address::new(393_216)?, 256)?,
            _ => invalid.page = Page::new(Address::new(512)?, 128)?,
        }
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect(b"FV\r", b"FV 1.00\r");
        mock.expect(b"TY\r", b"TY K,2,1\r");
        mock.expect(b"0M PROGRAM\r", b"0M\r");
        mock.expect(b"E", &[crate::protocol::mcp::ACK]);
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let mut called = false;
        let result = session
            .compare_exchange_pages(
                &[first, invalid],
                |_| {
                    called = true;
                    Ok(())
                },
                |_| {},
            )
            .await;
        assert!(
            matches!(
                result,
                Err(Error::Mcp(
                    McpError::ReplacementLength { .. }
                        | McpError::PageNotWritable { .. }
                        | McpError::NonCanonicalPage { .. }
                ))
            ),
            "malformed later entries must fail before reading an earlier valid entry"
        );
        assert!(!called, "invalid batches must never reach durable intent");
        assert!(
            session.journal().possibly_written.is_empty(),
            "invalid batches cannot change the journal"
        );
        session.exit().await?;
        radio.into_transport().assert_complete();
    }
    Ok(())
}
