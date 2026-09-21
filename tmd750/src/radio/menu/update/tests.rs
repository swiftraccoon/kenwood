//! Pure assignment, whole-page binding, and active-state guard tests.

use super::*;
use crate::types::{FirmwareIdentity, RadioType};

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

fn identity() -> Result<Identity, TestError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn assignment(name: &str, slot: u8, text: &str) -> Result<MenuAssignment, TestError> {
    Ok(MenuAssignment::new(
        name,
        Some(SlotIndex::new(slot)?),
        text,
    )?)
}

fn fixture(
    assignments: &[MenuAssignment],
    active: SlotIndex,
    fill: u8,
) -> Result<MenuFieldSnapshot, TestError> {
    let mut pages = BTreeMap::new();
    let mut required = vec![
        Page::new(Address::new(8)?, 40)?,
        Page::new(Address::new(323_584)?, 256)?,
        Page::new(Address::new(331_776 + active.offset())?, 256)?,
    ];
    for assignment in assignments {
        required.extend(assignment.selection().pages()?);
    }
    for page in required {
        let _previous = pages.insert(page.address().as_u32(), (page, vec![fill; page.len()]));
    }
    let snapshot = MenuFieldSnapshot::from_pages(pages.into_values().collect())?;
    let snapshot = changed(&snapshot, 10, 0)?;
    let snapshot = changed(&snapshot, 323_593, active.index())?;
    changed(&snapshot, 331_776 + active.offset(), 0)
}

fn changed(
    snapshot: &MenuFieldSnapshot,
    address: u32,
    value: u8,
) -> Result<MenuFieldSnapshot, TestError> {
    let mut pages = snapshot.pages().to_vec();
    let (page, bytes) = pages
        .iter_mut()
        .find(|(page, _bytes)| (page.address().as_u32()..page.region().end()).contains(&address))
        .ok_or("fixture mutation requires a captured page")?;
    let offset = usize::try_from(address - page.address().as_u32())?;
    *bytes
        .get_mut(offset)
        .ok_or("fixture mutation exceeds its page")? = value;
    Ok(MenuFieldSnapshot::from_pages(pages)?)
}

#[test]
fn assignments_resolve_canonical_names_preserve_text_and_enforce_scope() -> TestResult {
    let text = String::from("  Mixed Case  ");
    let value = MenuAssignment::new("PM.PMNAME2", None, &text)?;
    drop(text);
    assert_eq!(
        value.field().descriptor.name,
        "pm.PmName2",
        "case-insensitive lookup must retain the canonical registry field"
    );
    assert_eq!(
        value.slot(),
        None,
        "global assignments must retain their slot-free scope"
    );
    assert_eq!(
        value.value(),
        &DecodedFieldValue::Text("  Mixed Case  ".to_owned()),
        "assignments must own exact input text, including edge spaces"
    );
    let per_slot = assignment("radio.Beep", 5, "off")?;
    assert_eq!(
        per_slot.value(),
        &DecodedFieldValue::Bool(false),
        "boolean aliases must become typed values"
    );
    assert_eq!(
        per_slot.slot(),
        Some(SlotIndex::new(5)?),
        "the selected PM must remain explicit"
    );
    assert!(
        matches!(
            MenuAssignment::new("radio.Beep", None, "true"),
            Err(MenuUpdateError::Schema(SchemaError::SlotRequired { .. }))
        ),
        "per-slot fields cannot silently default to PM Off"
    );
    assert!(
        matches!(
            MenuAssignment::new("pm.PmName2", Some(SlotIndex::new(0)?), "X"),
            Err(MenuUpdateError::Schema(SchemaError::UnexpectedSlot { .. }))
        ),
        "global fields cannot ignore a supplied slot"
    );
    for text in ["A\0B", "12345678901234567"] {
        assert!(
            matches!(
                MenuAssignment::new("pm.PmName2", None, text),
                Err(MenuUpdateError::Value(_))
            ),
            "non-round-tripping or oversized text must fail before a plan exists"
        );
    }
    Ok(())
}

#[test]
fn policies_supplemental_domains_and_unknown_names_fail_without_input_echo() -> TestResult {
    let secret = "unretained-secret-value";
    let unknown = MenuAssignment::new(secret, None, secret);
    let error = unknown.err().ok_or("unregistered field was admitted")?;
    assert!(
        matches!(error, MenuUpdateError::UnknownField),
        "unknown names must have a distinct admission error"
    );
    assert!(
        !format!("{error:?}: {error}").contains(secret),
        "unknown-name diagnostics must not echo potentially sensitive input"
    );
    for (name, policy) in [
        ("radio.PoweronBitmap", MenuWritePolicy::Binary),
        ("radio.TimeZone", MenuWritePolicy::UnresolvedDomain),
        (
            "dv.DvGatewayModeDvGateway",
            MenuWritePolicy::LifecycleRequired,
        ),
        ("pm.PmSelect", MenuWritePolicy::LifecycleRequired),
        ("radio.UsbFunction", MenuWritePolicy::LifecycleRequired),
    ] {
        let result = MenuAssignment::new(name, Some(SlotIndex::new(0)?), secret);
        let error = result
            .err()
            .ok_or("excluded policy admitted an assignment")?;
        assert!(
            matches!(&error, MenuUpdateError::NotOrdinary { policy: actual, .. } if *actual == policy),
            "field policy must reject before parsing its supplied value"
        );
        assert!(
            !format!("{error:?}: {error}").contains(secret),
            "policy rejection must not retain the supplied text"
        );
    }
    let linked = assignment("radio.GroupLink0", 0, "255")?;
    assert_eq!(
        linked.value(),
        &DecodedFieldValue::Unsigned(255),
        "the known not-linked sentinel must remain available"
    );
    for (name, raw) in [
        ("radio.GroupLink0", "30"),
        ("dv.MyCallsignSelectDvGateway", "6"),
    ] {
        assert!(
            matches!(
                MenuAssignment::new(name, Some(SlotIndex::new(0)?), raw),
                Err(MenuUpdateError::Policy(MenuWritePolicyError::Schema(
                    SchemaError::DisallowedValue { .. }
                )))
            ),
            "supplemental semantic domains must apply after storage parsing"
        );
    }
    Ok(())
}

#[test]
fn multiple_groups_and_slots_merge_shared_bits_without_replacing_guard_page_changes() -> TestResult
{
    let assignments = vec![
        assignment("radio.TxEqualizerFmNfm", 0, "on")?,
        assignment("radio.TxEqualizerDv", 0, "off")?,
        assignment("radio.TxEqualizerFmNfm", 5, "off")?,
        assignment("gps.MyPositionList[0].Altitude", 5, "-500")?,
        MenuAssignment::new("pm.PmName2", None, " HOME ")?,
        assignment(
            "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
            2,
            "KQ4NIT",
        )?,
    ];
    let snapshot = fixture(&assignments, SlotIndex::new(2)?, 0xA5)?;
    let original = snapshot.clone();
    let plan = MenuUpdatePlan::new(&identity()?, &snapshot, assignments.clone())?;
    assert_eq!(
        plan.assignments(),
        assignments,
        "the immutable plan must retain all requested groups and PM scopes in caller order"
    );
    assert_eq!(
        plan.identity(),
        &identity()?,
        "the plan must retain the complete captured identity"
    );
    let actual_pages: Vec<_> = plan
        .replacements()
        .iter()
        .map(|change| change.page().address().as_u32())
        .collect();
    assert_eq!(
        actual_pages,
        vec![8, 323_584, 328_960, 348_160, 369_920, 370_176],
        "changes and guards must be deduplicated and ordered by canonical page address"
    );
    let preview = MenuFieldSnapshot::from_pages(
        plan.replacements()
            .iter()
            .map(|change| (change.page(), change.replacement().to_vec()))
            .collect(),
    )?;
    for requested in &assignments {
        assert_eq!(
            preview.value(requested.selection())?,
            *requested.value(),
            "every requested typed value must survive merged page planning"
        );
    }
    for change in plan.replacements() {
        assert_eq!(
            Some(change.expected()),
            snapshot.page(change.page()),
            "every complete before-image must remain exactly captured"
        );
        if change.page().address().as_u32() == 328_960 {
            for (offset, byte) in change.replacement().iter().enumerate() {
                assert_eq!(
                    *byte,
                    if offset == 37 { 0xA3 } else { 0xA5 },
                    "only the two independently owned equalizer bits may change in this page"
                );
            }
        }
    }
    assert_eq!(
        snapshot, original,
        "plan preparation must not modify any source page or coverage"
    );
    assert_eq!(
        preview.value(guard_selection(PM_FIELD, None)?)?,
        DecodedFieldValue::Unsigned(2),
        "PM-name writes sharing the control page must preserve its active selector"
    );
    assert_eq!(
        preview.value(guard_selection(GATEWAY_FIELD, Some(SlotIndex::new(2)?))?)?,
        DecodedFieldValue::Unsigned(0),
        "MY1 writes sharing the active Gateway page must preserve Gateway Off"
    );
    Ok(())
}

#[test]
fn exact_identity_format_active_pm_and_gateway_are_independent_guards() -> TestResult {
    let assignments = vec![assignment("radio.Beep", 0, "off")?];
    let baseline = fixture(&assignments, SlotIndex::new(0)?, 0)?;
    for firmware in ["1.00", "1.2", "1.02x"] {
        let mut wrong = identity()?;
        wrong.firmware = FirmwareIdentity::new(firmware)?;
        assert!(
            matches!(MenuUpdatePlan::new(&wrong, &baseline, assignments.clone()), Err(MenuUpdateError::UnsupportedIdentity { actual }) if actual == wrong),
            "firmware admission must match the entire exact identity, not a version prefix or ordering"
        );
    }
    for radio_type in ["J,2,1", "K,2,2", "K,2,1x"] {
        let mut wrong = identity()?;
        wrong.radio_type = RadioType::new(radio_type)?;
        assert!(
            matches!(MenuUpdatePlan::new(&wrong, &baseline, assignments.clone()), Err(MenuUpdateError::UnsupportedIdentity { actual }) if actual == wrong),
            "every opaque radio-type component must remain exact"
        );
    }
    for actual in [1, 255] {
        assert!(
            matches!(MenuUpdatePlan::new(&identity()?, &changed(&baseline, 10, actual)?, assignments.clone()), Err(MenuUpdateError::MemoryFormat { actual: observed }) if observed == actual),
            "actual captured format must be zero before layout admission"
        );
    }
    for actual in [6, 255] {
        assert!(
            matches!(MenuUpdatePlan::new(&identity()?, &changed(&baseline, 323_593, actual)?, assignments.clone()), Err(MenuUpdateError::PmSelection { actual: observed }) if observed == u64::from(actual)),
            "an unknown active PM must not select a default bank"
        );
    }
    for slot in SlotIndex::all() {
        let snapshot = fixture(&assignments, slot, 0)?;
        let plan = MenuUpdatePlan::new(&identity()?, &snapshot, assignments.clone())?;
        assert!(
            plan.replacements()
                .iter()
                .any(|change| change.page().address().as_u32() == 331_776 + slot.offset()),
            "each active PM requires its own complete Gateway guard page"
        );
        for raw in [1, 2, 255] {
            let snapshot = changed(&snapshot, 331_776 + slot.offset(), raw)?;
            assert!(
                matches!(MenuUpdatePlan::new(&identity()?, &snapshot, assignments.clone()), Err(MenuUpdateError::GatewayMode { slot: actual_slot, actual }) if actual_slot == slot && actual == DvGatewayMode::from(raw)),
                "Gateway admission must follow active PM, rejecting Terminal and unknown states"
            );
        }
    }
    Ok(())
}

#[test]
fn missing_whole_pages_empty_requests_and_duplicate_assignments_are_rejected() -> TestResult {
    let requested = assignment("radio.Beep", 0, "on")?;
    let assignments = vec![requested.clone()];
    let snapshot = fixture(&assignments, SlotIndex::new(0)?, 0)?;
    for (missing, _bytes) in snapshot.pages() {
        let partial = MenuFieldSnapshot::from_pages(
            snapshot
                .pages()
                .iter()
                .filter(|(page, _)| page != missing)
                .cloned()
                .collect(),
        )?;
        assert!(
            matches!(
                MenuUpdatePlan::new(&identity()?, &partial, assignments.clone()),
                Err(
                    MenuUpdateError::Schema(SchemaError::SnapshotPageMissing { .. })
                        | MenuUpdateError::Operation(Error::Schema(
                            SchemaError::SnapshotPageMissing { .. }
                        ))
                )
            ),
            "every changed or guard page must be completely captured; no missing page may be synthesized"
        );
    }
    assert!(
        matches!(
            MenuUpdatePlan::new(&identity()?, &snapshot, Vec::new()),
            Err(MenuUpdateError::EmptyAssignments)
        ),
        "an empty request is not an ordinary update plan"
    );
    for duplicate in [requested.clone(), assignment("RADIO.BEEP", 0, "off")?] {
        assert!(
            matches!(MenuUpdatePlan::new(&identity()?, &snapshot, vec![requested.clone(), duplicate]), Err(MenuUpdateError::DuplicateAssignment { field: "radio.Beep", slot: Some(slot) }) if slot == SlotIndex::new(0)?),
            "duplicate canonical field and slot must be refused regardless of casing or value agreement"
        );
    }
    let page = Page::new(Address::new(8)?, 40)?;
    assert!(
        matches!(
            MenuFieldSnapshot::from_pages(vec![(page, vec![0; 39])]),
            Err(SchemaError::SnapshotPageLength { .. })
        ),
        "an incomplete format fragment must not be constructible as captured coverage"
    );
    Ok(())
}

#[test]
fn nonempty_noop_plans_retain_all_expected_pages_as_comparisons() -> TestResult {
    let assignments = vec![assignment("radio.Beep", 0, "off")?];
    let snapshot = fixture(&assignments, SlotIndex::new(0)?, 0)?;
    let plan = MenuUpdatePlan::new(&identity()?, &snapshot, assignments)?;
    assert_eq!(
        plan.replacements().len(),
        4,
        "a no-op still compares the field page and all three complete guard pages"
    );
    assert!(
        plan.replacements().iter().all(PageReplacement::is_noop),
        "equal desired values must never manufacture a write"
    );
    for change in plan.replacements() {
        assert_eq!(
            Some(change.expected()),
            snapshot.page(change.page()),
            "no-op plans must retain complete captured pages, not just decoded values"
        );
    }
    plan.validate_identity(&identity()?)?;
    let mut wrong = identity()?;
    wrong.radio_type = RadioType::new("K,2,2")?;
    assert!(
        matches!(plan.validate_identity(&wrong), Err(MenuUpdateError::IdentityMismatch { expected, actual }) if expected == identity()? && actual == wrong),
        "even a compare-only plan must reject current identity mismatch"
    );
    Ok(())
}

#[test]
fn unknown_current_enum_remains_an_exact_before_image_for_a_supported_replacement() -> TestResult {
    let requested = assignment("radio.MeterType", 0, "1")?;
    let snapshot = fixture(std::slice::from_ref(&requested), SlotIndex::new(0)?, 255)?;
    assert_eq!(
        snapshot.value(requested.selection())?,
        DecodedFieldValue::Unsigned(255),
        "the source must preserve an unknown stored enum without replacing it with a default"
    );
    let plan = MenuUpdatePlan::new(&identity()?, &snapshot, vec![requested.clone()])?;
    let page = *requested
        .selection()
        .pages()?
        .first()
        .ok_or("meter field needs a page")?;
    let change = plan
        .replacements()
        .iter()
        .find(|change| change.page() == page)
        .ok_or("meter page needs a replacement")?;
    let offset = requested
        .field()
        .descriptor
        .address(requested.slot())?
        .as_usize()
        - page.address().as_usize();
    assert_eq!(
        change.expected().get(offset),
        Some(&255),
        "unknown current value must remain part of the immutable whole-page comparison"
    );
    assert_eq!(
        change.replacement().get(offset),
        Some(&1),
        "valid desired choices must be able to replace unknown stored values"
    );
    assert!(
        matches!(
            MenuAssignment::new("radio.MeterType", Some(SlotIndex::new(0)?), "255"),
            Err(MenuUpdateError::Value(MenuValueError::Schema(
                SchemaError::DisallowedValue { .. }
            )))
        ),
        "unknown current values must not become admissible desired choices"
    );
    Ok(())
}
