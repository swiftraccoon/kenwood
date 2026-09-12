//! Sparse menu coverage and protocol-order tests using synthetic transports.

use std::cell::Cell;
use std::collections::BTreeSet;

use super::*;
use crate::error::McpError;
use crate::memory::{
    Endian, FieldCodec, FieldDescriptor, FieldValue, MCP_D750_MENU_FIELDS, MenuValueError,
    PatchPlanner, SLOT_TERM, menu_field,
};
use crate::protocol::mcp::{ACK, read_request, write_request};
use crate::transport::MockTransport;
use crate::{Radio, Region};

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn field(name: &str) -> Result<&'static MenuField, TestError> {
    menu_field(name).ok_or_else(|| format!("missing registry field {name}").into())
}

fn page(address: u32, len: usize) -> Result<Page, TestError> {
    Ok(Page::new(Address::new(address)?, len)?)
}

const fn cross_page_field(base: u32, per_slot: bool) -> MenuField {
    MenuField {
        menu: "test",
        enum_type: None,
        descriptor: FieldDescriptor::with_terms(
            "test.CrossPage",
            base,
            if per_slot { &[SLOT_TERM] } else { &[] },
            FieldCodec::Unsigned {
                width: 4,
                endian: Endian::Little,
                min: 0,
                max: 0xFFFF_FFFF,
            },
        ),
        options: &[],
        allowed_values: &[],
        storage_transform: None,
        is_blob: false,
    }
}

fn entry_mock() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.00\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock
}

fn queue_read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    let mut reply = write_request(page).to_vec();
    reply.extend_from_slice(bytes);
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

fn queue_verified_write(mock: &mut MockTransport, replacement: &PageReplacement) {
    let mut frame = write_request(replacement.page()).to_vec();
    frame.extend_from_slice(replacement.replacement());
    mock.expect(&frame, &[ACK]);
    queue_read(mock, replacement.page(), replacement.replacement());
}

#[test]
fn registry_scopes_require_explicit_slots_and_resolve_canonical_scalar_pages() -> TestResult {
    let canonical: BTreeSet<_> = regions::menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .map(|page| (page.address().as_u32(), page.len()))
        .collect();
    for field in MCP_D750_MENU_FIELDS.iter().filter(|field| !field.is_blob) {
        let scopes = if field.descriptor.is_per_slot() {
            assert!(
                matches!(
                    ScopedMenuField::new(field, None),
                    Err(SchemaError::SlotRequired { .. })
                ),
                "{} must not default to PM Off",
                field.descriptor.name
            );
            SlotIndex::all().map(Some).to_vec()
        } else {
            assert!(
                matches!(
                    ScopedMenuField::new(field, Some(SlotIndex::new(0)?)),
                    Err(SchemaError::UnexpectedSlot { .. })
                ),
                "{} must reject an irrelevant slot",
                field.descriptor.name
            );
            vec![None]
        };
        for slot in scopes {
            let selection = ScopedMenuField::new(field, slot)?;
            assert_eq!(
                selection.field(),
                field,
                "scope must retain the exact descriptor"
            );
            assert_eq!(
                selection.slot(),
                slot,
                "scope must retain the explicit slot"
            );
            let pages = selection.pages()?;
            let first = pages.first().ok_or("scalar field must occupy a page")?;
            let last = pages.last().ok_or("scalar field must occupy a page")?;
            let start = field.descriptor.address(slot)?;
            let end = start.as_usize() + field.descriptor.codec.encoded_len();
            assert!(
                first.region().contains(start),
                "{} first page must cover its start",
                field.descriptor.name
            );
            assert!(
                usize::try_from(last.region().end())? >= end,
                "{} last page must cover its complete codec",
                field.descriptor.name
            );
            for page in &pages {
                assert!(
                    canonical.contains(&(page.address().as_u32(), page.len())),
                    "{} must resolve only complete transfer units",
                    field.descriptor.name
                );
            }
            for pair in pages.windows(2) {
                let [left, right] = pair else {
                    return Err("page windows must contain two entries".into());
                };
                assert_eq!(
                    left.region().end(),
                    right.address().as_u32(),
                    "field coverage must contain no unread gap"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn bitmap_slots_resolve_separate_read_only_spans() -> TestResult {
    let bitmap = field("radio.PoweronBitmap")?;
    for slot in SlotIndex::all() {
        let pages = ScopedMenuField::new(bitmap, Some(slot))?.pages()?;
        assert_eq!(
            pages.len(),
            1_000,
            "each explicit bitmap slot has 256,000 bytes"
        );
        let start = 393_216 + u32::from(slot.index()) * 256_000;
        for (index, page) in pages.iter().enumerate() {
            assert_eq!(
                page.address().as_u32(),
                start + u32::try_from(index)? * 256,
                "bitmap pages must stay in their own slot and transfer order"
            );
            assert_eq!(page.len(), 256, "bitmap transfer units are full pages");
            assert!(
                !regions::is_writable_page(*page),
                "bitmap read coverage must not grant write authority"
            );
        }
    }
    Ok(())
}

#[test]
fn sparse_snapshots_reject_partial_duplicate_noncanonical_and_gap_coverage() -> TestResult {
    let first = page(327_936, 256)?;
    let second = page(328_192, 256)?;
    let field = cross_page_field(328_190, true);
    let selection = ScopedMenuField::new(&field, Some(SlotIndex::new(0)?))?;
    let partial = MenuFieldSnapshot::from_pages(vec![(first, vec![0; 256])])?;
    assert!(
        matches!(
            partial.value(selection),
            Err(SchemaError::SnapshotPageMissing {
                address: 328_192,
                len: 256,
                ..
            })
        ),
        "zero-filled decode storage must never stand in for an unread second page"
    );
    assert!(
        partial.page(page(327_936, 128)?).is_none(),
        "a captured whole page is not a separately captured fragment"
    );
    for len in [0, 255, 257] {
        assert!(
            matches!(MenuFieldSnapshot::from_pages(vec![(first, vec![0; len])]), Err(SchemaError::SnapshotPageLength { expected: 256, actual, .. }) if actual == len),
            "captured bytes must fill the declared canonical page exactly"
        );
    }
    assert!(
        matches!(
            MenuFieldSnapshot::from_pages(vec![
                (first, vec![0; 256]),
                (second, vec![0; 256]),
                (first, vec![0; 256])
            ]),
            Err(SchemaError::DuplicateSnapshotPage { address: 327_936 })
        ),
        "even identical nonadjacent duplicate claims must be refused"
    );
    for (address, len) in [(48, 8), (512, 128), (513, 255), (327_680, 256)] {
        assert!(
            matches!(
                MenuFieldSnapshot::from_pages(vec![(page(address, len)?, vec![0; len])]),
                Err(SchemaError::SnapshotPageNotCanonical { .. })
            ),
            "unknown ranges and partial or shifted pages must not become coverage"
        );
    }
    let gap = cross_page_field(46, false);
    assert!(
        matches!(
            ScopedMenuField::new(&gap, None)?.pages(),
            Err(SchemaError::SnapshotPageNotCanonical { address: 48, .. })
        ),
        "a descriptor crossing a transfer gap must fail before reading"
    );
    let complete =
        MenuFieldSnapshot::from_pages(vec![(second, vec![2; 256]), (first, vec![1; 256])])?;
    assert_eq!(
        complete.pages().first().map(|(page, _)| *page),
        Some(first),
        "sparse input must be sorted without filling absent pages"
    );
    assert_eq!(
        complete.value(selection)?,
        DecodedFieldValue::Unsigned(0x0202_0101),
        "a scalar spanning two captured pages must decode in storage byte order"
    );
    let fragment = page(327_681, 255)?;
    assert_eq!(
        MenuFieldSnapshot::from_pages(vec![(fragment, vec![7; 255])])?.page(fragment),
        Some(&[7; 255][..]),
        "nonaligned canonical fragments must remain admissible"
    );
    Ok(())
}

#[test]
fn planning_preserves_shared_bits_unrelated_pages_and_immutable_source() -> TestResult {
    let slot = Some(SlotIndex::new(0)?);
    let fm = field("radio.TxEqualizerFmNfm")?;
    let dv = field("radio.TxEqualizerDv")?;
    let selected = ScopedMenuField::new(fm, slot)?;
    let target = *selected
        .pages()?
        .first()
        .ok_or("equalizer must occupy a page")?;
    let other = page(8, 40)?;
    let snapshot = MenuFieldSnapshot::from_pages(vec![
        (target, vec![0xA5; target.len()]),
        (other, vec![0x5A; other.len()]),
    ])?;
    let original = snapshot.clone();
    let mut planner = PatchPlanner::new();
    let _planned = planner.set_menu(fm, slot, FieldValue::Bool(true))?;
    let _planned = planner.set_menu(dv, slot, FieldValue::Bool(false))?;
    let patches = planner.finish()?;
    let changes = snapshot.plan_exchanges(&patches)?;
    assert_eq!(
        changes.len(),
        1,
        "shared-byte edits must merge into one whole-page exchange"
    );
    let change = changes.first().ok_or("merged change must exist")?;
    let offset = fm.descriptor.address(slot)?.as_usize() - target.address().as_usize();
    for (index, (&before, &after)) in change
        .expected()
        .iter()
        .zip(change.replacement())
        .enumerate()
    {
        assert_eq!(
            before, 0xA5,
            "complete expected bytes must retain the captured before-image"
        );
        assert_eq!(
            after,
            if index == offset { 0xA3 } else { 0xA5 },
            "only the two owned bits may change at byte {index}"
        );
    }
    let preview = snapshot.patched(&patches)?;
    assert_eq!(
        snapshot, original,
        "planning and preview must never mutate their source snapshot"
    );
    assert_eq!(
        preview.page(other),
        original.page(other),
        "preview must retain unrelated captured pages byte-for-byte"
    );
    assert_eq!(
        preview.value(selected)?,
        DecodedFieldValue::Bool(true),
        "preview must decode the planned FM bit"
    );
    assert_eq!(
        preview.value(ScopedMenuField::new(dv, slot)?)?,
        DecodedFieldValue::Bool(false),
        "preview must decode the independent DV bit"
    );
    Ok(())
}

#[test]
fn unknown_stored_enum_is_readable_but_not_a_new_menu_choice() -> TestResult {
    let field = field("radio.RepeaterMode")?;
    let selection = ScopedMenuField::new(field, None)?;
    let target = *selection
        .pages()?
        .first()
        .ok_or("repeater mode must occupy a page")?;
    let snapshot = MenuFieldSnapshot::from_pages(vec![(target, vec![255; target.len()])])?;
    assert_eq!(
        snapshot.value(selection)?,
        DecodedFieldValue::Unsigned(255),
        "unknown stored choices must survive diagnostic reads without a substituted default"
    );
    assert!(
        matches!(
            field.parse_value("255"),
            Err(MenuValueError::Schema(SchemaError::DisallowedValue { .. }))
        ),
        "readable unknown values must not bypass writable menu-domain admission"
    );
    Ok(())
}

#[tokio::test]
async fn reading_cross_page_fields_deduplicates_and_orders_two_pm_slots() -> TestResult {
    let field = cross_page_field(328_190, true);
    let slot0 = ScopedMenuField::new(&field, Some(SlotIndex::new(0)?))?;
    let slot1 = ScopedMenuField::new(&field, Some(SlotIndex::new(1)?))?;
    let expected = [
        page(327_936, 256)?,
        page(328_192, 256)?,
        page(336_128, 256)?,
        page(336_384, 256)?,
    ];
    let mut mock = entry_mock();
    for (index, page) in expected.iter().enumerate() {
        queue_read(
            &mut mock,
            *page,
            &vec![u8::try_from(index + 1)?; page.len()],
        );
    }
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut progress = Vec::new();
    let snapshot = session
        .read_menu_snapshot(&[slot1, slot0, slot1, slot0], |event| progress.push(event))
        .await?;
    assert_eq!(
        snapshot
            .pages()
            .iter()
            .map(|(page, _)| *page)
            .collect::<Vec<_>>(),
        expected,
        "repeated selections must read each complete page once in address order"
    );
    assert_eq!(
        snapshot.value(slot0)?,
        DecodedFieldValue::Unsigned(0x0202_0101),
        "PM Off must decode only its own captured pages"
    );
    assert_eq!(
        snapshot.value(slot1)?,
        DecodedFieldValue::Unsigned(0x0404_0303),
        "PM1 must decode only its own captured pages"
    );
    assert_eq!(
        progress,
        (1..=4)
            .map(|done| Progress { done, total: 4 })
            .collect::<Vec<_>>(),
        "progress must count deduplicated transfer units rather than fields"
    );
    assert!(
        session.journal().possibly_written.is_empty(),
        "menu reads cannot create write debt"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn missing_snapshot_and_late_invalid_selection_fail_before_page_traffic() -> TestResult {
    let fm = field("radio.TxEqualizerFmNfm")?;
    let selection = ScopedMenuField::new(fm, Some(SlotIndex::new(0)?))?;
    let gap = cross_page_field(46, false);
    let mut planner = PatchPlanner::new();
    let _planned = planner.set_menu(fm, selection.slot(), FieldValue::Bool(true))?;
    let patches = planner.finish()?;
    let empty = MenuFieldSnapshot::from_pages(Vec::new())?;
    let mut mock = entry_mock();
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let callbacks = Cell::new(0);
    let result = session
        .compare_exchange_menu_patches(
            &patches,
            &empty,
            |_| {
                callbacks.set(callbacks.get() + 1);
                Ok(())
            },
            |_| {
                callbacks.set(callbacks.get() + 1);
            },
        )
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Schema(SchemaError::SnapshotPageMissing {
                field: "menu patch",
                ..
            }))
        ),
        "missing patch coverage must fail before live comparison or write intent"
    );
    let result = session
        .read_menu_snapshot(&[selection, ScopedMenuField::new(&gap, None)?], |_| {
            callbacks.set(callbacks.get() + 1);
        })
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Schema(SchemaError::SnapshotPageNotCanonical {
                address: 48,
                ..
            }))
        ),
        "all field spans must be validated before reading even the first valid selection"
    );
    assert_eq!(
        callbacks.get(),
        0,
        "invalid requests must emit neither intent nor progress"
    );
    assert!(
        session
            .read_menu_snapshot(&[], |_| {})
            .await?
            .pages()
            .is_empty(),
        "empty valid reads must remain traffic-free"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

fn two_slot_patch(second_value: bool) -> Result<(MenuFieldSnapshot, PatchSet), TestError> {
    let field = field("radio.TxEqualizerFmNfm")?;
    let mut pages = Vec::new();
    let mut planner = PatchPlanner::new();
    for (slot, value) in [
        (SlotIndex::new(0)?, true),
        (SlotIndex::new(1)?, second_value),
    ] {
        let selection = ScopedMenuField::new(field, Some(slot))?;
        let page = *selection
            .pages()?
            .first()
            .ok_or("equalizer must occupy a page")?;
        pages.push((page, vec![0; page.len()]));
        let _planned = planner.set_menu(field, Some(slot), FieldValue::Bool(value))?;
    }
    Ok((MenuFieldSnapshot::from_pages(pages)?, planner.finish()?))
}

#[tokio::test]
async fn menu_batch_compares_every_page_before_writing_and_skips_noops() -> TestResult {
    let (snapshot, patches) = two_slot_patch(false)?;
    let exchanges = snapshot.plan_exchanges(&patches)?;
    let [changed, unchanged] = exchanges.as_slice() else {
        return Err("two PM slots must produce two page plans".into());
    };
    assert!(
        !changed.is_noop() && unchanged.is_noop(),
        "fixture must contain one change and one compared no-op"
    );
    let mut mock = entry_mock();
    for change in &exchanges {
        queue_read(&mut mock, change.page(), change.expected());
    }
    queue_verified_write(&mut mock, changed);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut intents = Vec::new();
    let mut progress = Vec::new();
    let report = session
        .compare_exchange_menu_patches(
            &patches,
            &snapshot,
            |change| {
                intents.push(change.clone());
                Ok(())
            },
            |event| progress.push(event),
        )
        .await?;
    assert_eq!(
        report.compared_pages,
        vec![changed.page(), unchanged.page()],
        "all expected pages, including no-ops, must pass fresh comparison"
    );
    assert_eq!(
        report.verified_pages,
        vec![changed.page()],
        "only the changed page may be written and read back"
    );
    assert_eq!(
        report.unchanged_pages,
        vec![unchanged.page()],
        "freshly compared no-ops must be reported separately"
    );
    assert_eq!(
        intents,
        vec![changed.clone()],
        "the durable-intent callback must receive the exact complete change once"
    );
    assert_eq!(
        progress,
        vec![
            Progress { done: 1, total: 2 },
            Progress { done: 2, total: 2 }
        ],
        "completion progress must include the compared no-op"
    );
    assert_eq!(
        session.journal().possibly_written,
        vec![changed.page()],
        "no-op pages must not enter the conservative write journal"
    );
    assert_eq!(
        session.journal().verified,
        vec![changed.page()],
        "only complete immediate readback may mark the changed page verified"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn stale_later_page_prevents_every_write_in_the_menu_batch() -> TestResult {
    let (snapshot, patches) = two_slot_patch(true)?;
    let changes = snapshot.plan_exchanges(&patches)?;
    let [first, second] = changes.as_slice() else {
        return Err("two PM slots must produce two page plans".into());
    };
    let mut stale = second.expected().to_vec();
    *stale
        .last_mut()
        .ok_or("whole page must have a final byte")? = 1;
    let mut mock = entry_mock();
    queue_read(&mut mock, first.page(), first.expected());
    queue_read(&mut mock, second.page(), &stale);
    mock.expect(b"E", &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut called = false;
    let result = session
        .compare_exchange_menu_patches(
            &patches,
            &snapshot,
            |_| {
                called = true;
                Ok(())
            },
            |_| {},
        )
        .await;
    let Err(Error::Mcp(McpError::Interrupted {
        possibly_written,
        verified,
        source,
        ..
    })) = result
    else {
        return Err("stale page must return a typed interrupted comparison".into());
    };
    assert!(
        matches!(*source, Error::Mcp(McpError::CompareMismatch { address, offset }) if address == second.page().address().as_u32() && offset == second.page().len() - 1),
        "comparison must cover unrelated bytes through the end of every page"
    );
    assert_eq!(
        (possibly_written, verified),
        (0, 0),
        "a later stale page must stop the batch before an earlier page is written"
    );
    assert!(
        !called,
        "stale whole-page comparison must precede every durable write intent"
    );
    assert!(
        session.journal().possibly_written.is_empty(),
        "failed preflight cannot create write debt"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn uncertain_read_refuses_even_an_empty_menu_request_without_more_traffic() -> TestResult {
    let selection = ScopedMenuField::new(field("radio.RepeaterMode")?, None)?;
    let target = *selection
        .pages()?
        .first()
        .ok_or("repeater mode must occupy a page")?;
    let mut mock = entry_mock();
    mock.expect(&read_request(target), &[0; 5]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    assert!(
        session
            .read_menu_snapshot(&[selection], |_| {})
            .await
            .is_err(),
        "invalid read header must fail the exchange"
    );
    assert!(
        matches!(
            session.read_menu_snapshot(&[], |_| {}).await,
            Err(Error::Mcp(McpError::RecoveryRequired))
        ),
        "empty selection must not conceal recovery-required session state"
    );
    drop(session);
    radio.into_transport().assert_complete();
    Ok(())
}
