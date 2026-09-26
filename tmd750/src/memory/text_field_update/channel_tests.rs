use std::num::NonZeroU64;

use super::super::{
    ChannelNameUpdate, TextFieldUpdateError, TextFieldUpdateEvent,
    TextFieldUpdateSession as Session, TextFieldUpdateStatus,
};
use super::*;
use crate::types::{FirmwareIdentity, RadioModel, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    update: ChannelNameUpdate,
    identity: Identity,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
}

fn identity() -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "test ID must be nonzero".into())
}

fn channel(index: u16) -> Result<PhysicalChannel, Box<dyn std::error::Error>> {
    Ok(PhysicalChannel::new(index)?)
}

fn name(text: &str) -> Result<ChannelNameText, Box<dyn std::error::Error>> {
    Ok(ChannelNameText::new(text)?)
}

/// Channel 999's page: every other name byte distinctive, 999 unnamed.
fn unnamed_999_page() -> Result<[u8; PAGE_SIZE], Box<dyn std::error::Error>> {
    let mut page = [0xA5; PAGE_SIZE];
    page.get_mut(112..128).ok_or("channel 999 field")?.fill(0);
    Ok(page)
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let identity = identity()?;
    let original = unnamed_999_page()?;
    let update = ChannelNameUpdate::prepare(
        &identity,
        &original,
        channel(999)?,
        None,
        Some(&name("Repeater 1")?),
    )?;
    let desired = *update.desired_page();
    Ok(Fixture {
        update,
        identity,
        original,
        desired,
    })
}

fn fresh(fixture: &mut Fixture, session: u64) -> TestResult {
    fixture.update.record(TextFieldUpdateEvent::FreshSession {
        id: id(session)?,
        identity: &fixture.identity,
        memory_format: 0,
        whole_page: if session == 1 {
            &fixture.original
        } else {
            &fixture.desired
        },
        guards: (),
    })?;
    Ok(())
}

fn intent(fixture: &mut Fixture) -> TestResult {
    fixture
        .update
        .record(TextFieldUpdateEvent::DurableWriteIntent { id: id(1)? })?;
    Ok(())
}

fn readback(fixture: &mut Fixture) -> TestResult {
    fixture
        .update
        .record(TextFieldUpdateEvent::ImmediateReadback {
            whole_page: &fixture.desired,
        })?;
    Ok(())
}

fn finalize(fixture: &mut Fixture, session: u64) -> TestResult {
    fixture
        .update
        .record(TextFieldUpdateEvent::SessionFinalized {
            id: id(session)?,
            guards: (),
        })?;
    Ok(())
}

#[test]
fn names_reject_every_nonprintable_byte_and_pad_the_field_with_nul() -> TestResult {
    for text in ["", "12345678901234567", "CH\n1", "CH\0", "C\u{7f}H", "CHé"] {
        assert!(
            matches!(
                ChannelNameText::new(text),
                Err(TextFieldUpdateError::InvalidText {
                    field: "channel name",
                    ..
                })
            ),
            "{text:?}"
        );
    }
    for byte in 0..=u8::MAX {
        let bytes = [byte];
        if let Ok(text) = std::str::from_utf8(&bytes) {
            assert_eq!(
                ChannelNameText::new(text).is_ok(),
                (32..=126).contains(&byte)
            );
        }
    }
    for text in [" ", " CH 1 ", "1234567890123456", "!@#$%^&*()_+[]{}"] {
        assert_eq!(name(text)?.as_str(), text);
    }
    assert_eq!(name("Repeater 1")?.to_field(), *b"Repeater 1\0\0\0\0\0\0");
    assert_eq!(name("1234567890123456")?.to_field(), *b"1234567890123456");
    Ok(())
}

#[test]
fn required_page_follows_the_sixteen_name_grid_inside_the_writable_region() -> TestResult {
    for (index, start, offset) in [
        (0, 65_536, 0),
        (15, 65_536, 240),
        (16, 65_792, 0),
        (999, 81_408, 112),
        (1088, 82_944, 0),
        (1101, 82_944, 208),
    ] {
        let channel = channel(index)?;
        let page = ChannelNameUpdate::required_page(channel)?;
        assert_eq!(page.address().as_usize(), start, "channel {index} page");
        assert_eq!(page.len(), PAGE_SIZE, "channel {index} page length");
        assert_eq!(
            channel.name_address() - page.address().as_usize(),
            offset,
            "channel {index} offset"
        );
    }
    Ok(())
}

#[test]
fn preparation_changes_only_the_channel_field_and_keeps_the_shared_names() -> TestResult {
    // The last page also holds the weather-channel names beyond index 1101.
    let mut original = [0x11; PAGE_SIZE];
    original
        .get_mut(208..224)
        .ok_or("APRS name field")?
        .copy_from_slice(b"APRS Channel\0\0\0\0");
    original
        .get_mut(224..256)
        .ok_or("weather names")?
        .copy_from_slice(b"WX  1\0\0\0\0\0\0\0\0\0\0\0WX  2\0\0\0\0\0\0\0\0\0\0\0");
    let update = ChannelNameUpdate::prepare(
        &identity()?,
        &original,
        channel(1101)?,
        Some(&name("APRS Channel")?),
        Some(&name("Home APRS")?),
    )?;
    assert_eq!(update.channel(), channel(1101)?);
    assert_eq!(update.field(), TextField::ChannelName(channel(1101)?));
    assert_eq!(update.page().address().as_usize(), 82_944);
    assert_eq!(update.original_page(), &original);
    assert_eq!(
        update.current().map(ChannelNameText::as_str),
        Some("APRS Channel")
    );
    assert_eq!(
        update.requested().map(ChannelNameText::as_str),
        Some("Home APRS")
    );
    assert_eq!(
        update.desired_page().get(208..224),
        Some(b"Home APRS\0\0\0\0\0\0\0".as_slice())
    );
    for (index, (before, after)) in original.iter().zip(update.desired_page()).enumerate() {
        if !(208..224).contains(&index) {
            assert_eq!(before, after, "unrelated byte {index}");
        }
    }
    assert_eq!(update.status(), TextFieldUpdateStatus::NotWritten);
    assert_eq!(update.next_session()?, Session::Apply);
    Ok(())
}

#[test]
fn unnamed_is_sixteen_nuls_clearing_is_a_change_and_noops_are_rejected() -> TestResult {
    let identity = identity()?;
    let unnamed = unnamed_999_page()?;
    let update =
        ChannelNameUpdate::prepare(&identity, &unnamed, channel(999)?, None, Some(&name("X")?))?;
    assert_eq!(update.current(), None);
    assert_eq!(update.requested().map(ChannelNameText::as_str), Some("X"));
    assert_eq!(
        ChannelNameUpdate::prepare(
            &identity,
            &unnamed,
            channel(999)?,
            Some(&name("X")?),
            Some(&name("Y")?)
        )
        .err(),
        Some(TextFieldUpdateError::CurrentValueMismatch)
    );
    assert_eq!(
        ChannelNameUpdate::prepare(&identity, &unnamed, channel(999)?, None, None).err(),
        Some(TextFieldUpdateError::NoChange)
    );
    let mut named = unnamed;
    named
        .get_mut(112..128)
        .ok_or("channel 999 field")?
        .copy_from_slice(b"Y\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
    assert_eq!(
        ChannelNameUpdate::prepare(
            &identity,
            &named,
            channel(999)?,
            Some(&name("Y")?),
            Some(&name("Y")?)
        )
        .err(),
        Some(TextFieldUpdateError::NoChange)
    );
    let cleared =
        ChannelNameUpdate::prepare(&identity, &named, channel(999)?, Some(&name("Y")?), None)?;
    assert_eq!(cleared.requested(), None);
    assert_eq!(
        cleared.desired_page().get(112..128),
        Some([0; 16].as_slice())
    );
    let mut stray = named;
    *stray.get_mut(113).ok_or("padding byte")? = b'A';
    assert_eq!(
        ChannelNameUpdate::prepare(&identity, &stray, channel(999)?, Some(&name("Y")?), None).err(),
        Some(TextFieldUpdateError::CurrentValueMismatch),
        "padding must be exact"
    );
    Ok(())
}

#[test]
fn preparation_rejects_other_identities_and_partial_pages() -> TestResult {
    let unnamed = unnamed_999_page()?;
    for (firmware, radio_type) in [("1.03", "K,2,1"), ("1.02", "J,2,1")] {
        let identity = Identity {
            model: RadioModel::TmD750,
            firmware: FirmwareIdentity::new(firmware)?,
            radio_type: RadioType::new(radio_type)?,
        };
        assert_eq!(
            ChannelNameUpdate::prepare(&identity, &unnamed, channel(999)?, None, Some(&name("X")?))
                .err(),
            Some(TextFieldUpdateError::IdentityMismatch)
        );
    }
    for length in [0, 255, 257] {
        let short = vec![0; length];
        assert_eq!(
            ChannelNameUpdate::prepare(
                &identity()?,
                &short,
                channel(999)?,
                None,
                Some(&name("X")?)
            )
            .err(),
            Some(TextFieldUpdateError::PageLength { actual: length })
        );
    }
    Ok(())
}

#[test]
fn two_sessions_verify_across_sessions_and_intent_marks_a_possible_change() -> TestResult {
    let mut fixture = fixture()?;
    fresh(&mut fixture, 1)?;
    assert_eq!(fixture.update.status(), TextFieldUpdateStatus::NotWritten);
    assert_eq!(
        fixture.update.next_session(),
        Err(TextFieldUpdateError::UnexpectedEvent)
    );
    intent(&mut fixture)?;
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    readback(&mut fixture)?;
    finalize(&mut fixture, 1)?;
    assert_eq!(fixture.update.next_session()?, Session::Verify);
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    fresh(&mut fixture, 2)?;
    finalize(&mut fixture, 2)?;
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    assert_eq!(
        fixture.update.next_session(),
        Err(TextFieldUpdateError::TerminalState)
    );
    assert_eq!(
        fixture
            .update
            .record(TextFieldUpdateEvent::SessionFinalized {
                id: id(2)?,
                guards: (),
            }),
        Err(TextFieldUpdateError::TerminalState)
    );
    assert_eq!(
        fixture.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    Ok(())
}

#[test]
fn every_comparison_requires_the_exact_page_and_a_failure_keeps_the_status() -> TestResult {
    let mut drifted = fixture()?;
    let mut page = drifted.original;
    *page.get_mut(0).ok_or("first byte")? ^= 1;
    assert_eq!(
        drifted.update.record(TextFieldUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &drifted.identity,
            memory_format: 0,
            whole_page: &page,
            guards: (),
        }),
        Err(TextFieldUpdateError::PageMismatch)
    );
    assert_eq!(drifted.update.status(), TextFieldUpdateStatus::NotWritten);
    assert_eq!(
        drifted.update.next_session(),
        Err(TextFieldUpdateError::TerminalState)
    );

    let mut partial = fixture()?;
    assert_eq!(
        partial.update.record(TextFieldUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &partial.identity,
            memory_format: 0,
            whole_page: partial.original.get(..255).ok_or("partial page")?,
            guards: (),
        }),
        Err(TextFieldUpdateError::PageLength { actual: 255 })
    );

    let mut format = fixture()?;
    assert_eq!(
        format.update.record(TextFieldUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &format.identity,
            memory_format: 1,
            whole_page: &format.original,
            guards: (),
        }),
        Err(TextFieldUpdateError::MemoryFormat { actual: 1 })
    );

    let mut other = fixture()?;
    let stranger = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.03")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    assert_eq!(
        other.update.record(TextFieldUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &stranger,
            memory_format: 0,
            whole_page: &other.original,
            guards: (),
        }),
        Err(TextFieldUpdateError::IdentityMismatch)
    );

    let mut wrong_readback = fixture()?;
    fresh(&mut wrong_readback, 1)?;
    intent(&mut wrong_readback)?;
    let mut page = wrong_readback.desired;
    *page.get_mut(255).ok_or("last byte")? ^= 1;
    assert_eq!(
        wrong_readback
            .update
            .record(TextFieldUpdateEvent::ImmediateReadback { whole_page: &page }),
        Err(TextFieldUpdateError::PageMismatch)
    );
    assert_eq!(
        wrong_readback.update.status(),
        TextFieldUpdateStatus::PossiblyChanged,
        "a failed readback never clears the write risk"
    );
    assert_eq!(
        wrong_readback.update.next_session(),
        Err(TextFieldUpdateError::TerminalState)
    );

    let mut stale_verify = fixture()?;
    fresh(&mut stale_verify, 1)?;
    intent(&mut stale_verify)?;
    readback(&mut stale_verify)?;
    finalize(&mut stale_verify, 1)?;
    assert_eq!(
        stale_verify
            .update
            .record(TextFieldUpdateEvent::FreshSession {
                id: id(2)?,
                identity: &stale_verify.identity,
                memory_format: 0,
                whole_page: &stale_verify.original,
                guards: (),
            }),
        Err(TextFieldUpdateError::PageMismatch),
        "verification compares the desired page, not the original"
    );
    assert_eq!(
        stale_verify.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    Ok(())
}

#[test]
fn reused_sessions_out_of_order_events_and_wrong_finalization_ids_halt() -> TestResult {
    let mut early_intent = fixture()?;
    assert_eq!(
        intent(&mut early_intent)
            .err()
            .map(|error| error.to_string()),
        Some(TextFieldUpdateError::UnexpectedEvent.to_string())
    );
    assert_eq!(
        early_intent.update.status(),
        TextFieldUpdateStatus::NotWritten
    );

    let mut repeated_intent = fixture()?;
    fresh(&mut repeated_intent, 1)?;
    intent(&mut repeated_intent)?;
    assert_eq!(
        intent(&mut repeated_intent)
            .err()
            .map(|error| error.to_string()),
        Some(TextFieldUpdateError::UnexpectedEvent.to_string())
    );
    assert_eq!(
        repeated_intent.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );

    let mut wrong_id = fixture()?;
    fresh(&mut wrong_id, 1)?;
    intent(&mut wrong_id)?;
    readback(&mut wrong_id)?;
    assert_eq!(
        finalize(&mut wrong_id, 2)
            .err()
            .map(|error| error.to_string()),
        Some(TextFieldUpdateError::SessionMismatch.to_string())
    );
    assert_eq!(
        wrong_id.update.next_session(),
        Err(TextFieldUpdateError::TerminalState)
    );

    let mut reused = fixture()?;
    fresh(&mut reused, 1)?;
    intent(&mut reused)?;
    readback(&mut reused)?;
    finalize(&mut reused, 1)?;
    assert_eq!(
        reused.update.record(TextFieldUpdateEvent::FreshSession {
            id: id(1)?,
            identity: &reused.identity,
            memory_format: 0,
            whole_page: &reused.desired,
            guards: (),
        }),
        Err(TextFieldUpdateError::ReusedSession)
    );

    let mut skipped_readback = fixture()?;
    fresh(&mut skipped_readback, 1)?;
    intent(&mut skipped_readback)?;
    assert_eq!(
        finalize(&mut skipped_readback, 1)
            .err()
            .map(|error| error.to_string()),
        Some(TextFieldUpdateError::UnexpectedEvent.to_string())
    );
    Ok(())
}

#[test]
fn explicit_halt_is_irreversible_at_every_nonterminal_stage() -> TestResult {
    let mut before_any = fixture()?;
    before_any.update.halt();
    assert_eq!(
        before_any.update.next_session(),
        Err(TextFieldUpdateError::TerminalState)
    );
    assert_eq!(
        fresh(&mut before_any, 1)
            .err()
            .map(|error| error.to_string()),
        Some(TextFieldUpdateError::TerminalState.to_string())
    );
    assert_eq!(
        before_any.update.status(),
        TextFieldUpdateStatus::NotWritten
    );

    let mut after_intent = fixture()?;
    fresh(&mut after_intent, 1)?;
    intent(&mut after_intent)?;
    after_intent.update.halt();
    assert_eq!(
        after_intent.update.status(),
        TextFieldUpdateStatus::PossiblyChanged,
        "halting never clears an accepted intent"
    );
    assert_eq!(
        readback(&mut after_intent)
            .err()
            .map(|error| error.to_string()),
        Some(TextFieldUpdateError::TerminalState.to_string())
    );

    let mut complete = fixture()?;
    fresh(&mut complete, 1)?;
    intent(&mut complete)?;
    readback(&mut complete)?;
    finalize(&mut complete, 1)?;
    fresh(&mut complete, 2)?;
    finalize(&mut complete, 2)?;
    complete.update.halt();
    assert_eq!(
        complete.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions,
        "a completed update keeps its verified status"
    );
    Ok(())
}
