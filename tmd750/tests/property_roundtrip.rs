//! Property-based round trips for the TM-D750 value types, the CAT setter
//! echoes, the stored channel codecs and the `.d750` container.
//!
//! Every strategy generates values inside the documented domains, so a
//! failure is a codec or validator defect, not an out-of-domain input.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

// Dev-dependencies visible to every kenwood-tmd750 test target but unused
// here, acknowledged so `unused_crate_dependencies` stays silent.
use kenwood_schema as _;
use kenwood_transport as _;
use mcp_d75_extract as _;
use mmdvm as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::error::SchemaError;
use kenwood_tmd750::file::{ConfigHeader, FileLayout, HEADER_SIZE, RadioConfig, parse_d750};
use kenwood_tmd750::memory::{
    ChannelNameText, ConfigurationError, DecodedFieldValue, FieldCodec, MCP_D750_MENU_FIELDS,
    MemoryImage, MenuField, MenuWritePolicy, PatchPlanner, StandardConfiguration,
    StandardConfigurationDiff, TextImage, TextScope, TextScopeKind, TextSetting,
};
use kenwood_tmd750::protocol::cat::{Command, Response, parse_line};
use kenwood_tmd750::protocol::mcp::regions::{menu_regions, writable_page_for};
use kenwood_tmd750::protocol::mcp::{
    BytePatch, Header, HeaderCommand, PagePatch, read_request, write_request,
};
use kenwood_tmd750::types::{
    Address, AmHighCut, AprsCallsign, BacklightControl, Band, BandControl, BandDisplay,
    BeaconMethod, CHANNEL_NAME_SIZE, CrossToneField, DcsCode, DigitalCode, DigitalSquelch,
    DstarCallsignEntry, DstarSlot, FirmwareIdentity, Frequency, GpsSettings, IMAGE_LENGTH,
    IMAGE_LENGTH_U32, MemoryChannelAddress, MyPositionSelection, NmeaSentences, OffsetFrequency,
    OperatingMode, PacketDataRate, Page, PowerLevel, RadioType, Region, RouteField, SLOT_COUNT,
    SelectableMode, ShiftDirection, SlotIndex, SquelchLevel, StepSize, StoredCallsign,
    StoredChannel, StoredChannelFlag, ToneCode, ToneMode, TransmitSetting, TuningMode, VoxDelay,
    VoxGain, WIRE_DIGITS,
};
use kenwood_tmd750::{
    Identity, MenuAssignment, MenuFieldSnapshot, MenuUpdateError, MenuUpdatePlan, RadioModel,
};

/// Convert any debug-printable error into a `TestCaseError` so `?` can be used
/// inside `proptest!` blocks.
fn to_test_err<E: std::fmt::Debug>(error: E) -> TestCaseError {
    TestCaseError::fail(format!("{error:?}"))
}

// ============================================================================
// Value strategies
// ============================================================================

fn arb_band() -> impl Strategy<Value = Band> {
    prop_oneof![Just(Band::A), Just(Band::B)]
}

fn arb_frequency() -> impl Strategy<Value = Frequency> {
    (Frequency::MIN_HZ..=Frequency::MAX_HZ)
        .prop_filter_map("frequency inside the receiver range", |hz| {
            Frequency::new(hz).ok()
        })
}

fn arb_offset() -> impl Strategy<Value = OffsetFrequency> {
    (0..=OffsetFrequency::MAX_HZ).prop_filter_map("offset inside the configurable range", |hz| {
        OffsetFrequency::new(hz).ok()
    })
}

fn arb_step() -> impl Strategy<Value = StepSize> {
    proptest::sample::select(StepSize::ALL.to_vec())
}

fn arb_tone_mode() -> impl Strategy<Value = ToneMode> {
    prop_oneof![
        Just(ToneMode::Off),
        Just(ToneMode::Tone),
        Just(ToneMode::Ctcss),
        Just(ToneMode::Dcs),
        Just(ToneMode::CrossTone),
    ]
}

fn arb_tone_code() -> impl Strategy<Value = ToneCode> {
    (0..=ToneCode::MAX).prop_filter_map("tone table index", |index| ToneCode::new(index).ok())
}

fn arb_dcs_code() -> impl Strategy<Value = DcsCode> {
    (0..=DcsCode::MAX).prop_filter_map("DCS table index", |index| DcsCode::new(index).ok())
}

fn arb_shift() -> impl Strategy<Value = ShiftDirection> {
    prop_oneof![
        Just(ShiftDirection::Simplex),
        Just(ShiftDirection::Plus),
        Just(ShiftDirection::Minus),
    ]
}

fn arb_digital_squelch() -> impl Strategy<Value = DigitalSquelch> {
    prop_oneof![
        Just(DigitalSquelch::Off),
        Just(DigitalSquelch::Code),
        Just(DigitalSquelch::Callsign),
    ]
}

fn arb_callsign_field() -> impl Strategy<Value = StoredCallsign> {
    any::<[u8; 8]>().prop_map(StoredCallsign::from_bytes)
}

fn arb_transmit() -> impl Strategy<Value = TransmitSetting> {
    prop_oneof![
        arb_offset().prop_map(TransmitSetting::Offset),
        arb_frequency().prop_map(TransmitSetting::Split),
    ]
}

fn arb_dstar_slot() -> impl Strategy<Value = DstarSlot> {
    (DstarSlot::MIN..=DstarSlot::MAX).prop_filter_map("MY slot", |slot| DstarSlot::new(slot).ok())
}

/// A complete stored channel record built from in-domain components.
fn arb_stored_channel() -> impl Strategy<Value = StoredChannel> {
    let tuning = (
        arb_frequency(),
        arb_transmit(),
        arb_step(),
        arb_step(),
        0u8..=15,
        any::<bool>(),
        0u8..=7,
        arb_tone_mode(),
        any::<bool>(),
        arb_shift(),
    );
    let signaling = (
        arb_tone_code(),
        arb_tone_code(),
        arb_dcs_code(),
        (0..=CrossToneField::MAX)
            .prop_filter_map("cross tone nibble", |value| CrossToneField::new(value).ok()),
        (0..=RouteField::MAX).prop_filter_map("route field", |value| RouteField::new(value).ok()),
        arb_callsign_field(),
        arb_callsign_field(),
        arb_callsign_field(),
        arb_digital_squelch(),
        (0..=DigitalCode::MAX)
            .prop_filter_map("digital code", |value| DigitalCode::new(value).ok()),
    );
    (tuning, signaling).prop_map(
        |(
            (
                receive_frequency,
                transmit,
                receive_step,
                transmit_step,
                mode_nibble,
                mode_flag,
                mode_low_bits,
                tone_mode,
                reverse,
                shift,
            ),
            (
                tone_code,
                ctcss_code,
                dcs_code,
                cross_tone,
                route,
                ur_call,
                rpt1,
                rpt2,
                digital_squelch,
                digital_code,
            ),
        )| StoredChannel {
            receive_frequency,
            split: matches!(transmit, TransmitSetting::Split(_)),
            transmit,
            receive_step,
            transmit_step,
            mode: OperatingMode::from(mode_nibble),
            mode_flag,
            mode_low_bits,
            tone_mode,
            reverse,
            shift,
            tone_code,
            ctcss_code,
            dcs_code,
            cross_tone,
            route,
            ur_call,
            rpt1,
            rpt2,
            digital_squelch,
            digital_code,
        },
    )
}

fn arb_memory_address() -> impl Strategy<Value = MemoryChannelAddress> {
    prop_oneof![
        (0u16..=999).prop_filter_map("regular channel", |channel| {
            MemoryChannelAddress::regular(channel).ok()
        }),
        (0u8..=49).prop_filter_map("program scan lower", |index| {
            MemoryChannelAddress::program_scan_lower(index).ok()
        }),
        (0u8..=49).prop_filter_map("program scan upper", |index| {
            MemoryChannelAddress::program_scan_upper(index).ok()
        }),
        Just(MemoryChannelAddress::Priority),
    ]
}

fn arb_dstar_entry() -> impl Strategy<Value = DstarCallsignEntry> {
    (arb_dstar_slot(), "[ -~]{0,8}", "[ -~]{0,4}").prop_filter_map(
        "entry inside the accepted text domain",
        |(slot, callsign, memo)| DstarCallsignEntry::new(slot, &callsign, &memo).ok(),
    )
}

// ============================================================================
// Setter echo strategies: a command paired with the response its exact echo
// must parse to.
// ============================================================================

fn arb_band_setter_echo() -> impl Strategy<Value = (Command, Response)> {
    prop_oneof![
        (arb_band(), arb_frequency()).prop_map(|(band, frequency)| (
            Command::SetFrequency { band, frequency },
            Response::Frequency { band, frequency },
        )),
        (
            arb_band(),
            prop_oneof![
                Just(PowerLevel::High),
                Just(PowerLevel::Medium),
                Just(PowerLevel::Low)
            ]
        )
            .prop_map(|(band, level)| (
                Command::SetPowerLevel { band, level },
                Response::PowerLevel { band, level },
            )),
        (
            arb_band(),
            prop_oneof![
                Just(TuningMode::Vfo),
                Just(TuningMode::Memory),
                Just(TuningMode::Call),
                Just(TuningMode::DStarRepeater)
            ]
        )
            .prop_map(|(band, mode)| (
                Command::SetTuningMode { band, mode },
                Response::TuningMode { band, mode },
            )),
        (
            arb_band(),
            (0..=SquelchLevel::MAX)
                .prop_filter_map("squelch level", |level| SquelchLevel::new(level).ok())
        )
            .prop_map(|(band, level)| (
                Command::SetSquelch { band, level },
                Response::Squelch { band, level },
            )),
        (arb_band(), any::<bool>()).prop_map(|(band, enabled)| (
            Command::SetAttenuator { band, enabled },
            Response::Attenuator { band, enabled },
        )),
        (arb_band(), arb_step()).prop_map(|(band, step)| (
            Command::SetStepSize { band, step },
            Response::StepSize { band, step },
        )),
        (
            arb_band(),
            prop_oneof![
                Just(SelectableMode::Fm),
                Just(SelectableMode::Dv),
                Just(SelectableMode::Am),
                Just(SelectableMode::Nfm)
            ]
        )
            .prop_map(|(band, mode)| (
                Command::SetOperatingMode { band, mode },
                Response::OperatingMode {
                    band,
                    mode: OperatingMode::from(mode),
                },
            )),
        (arb_band(), arb_memory_address()).prop_map(|(band, address)| (
            Command::RecallMemoryChannel { band, address },
            Response::MemoryRecallAck { band, address },
        )),
    ]
}

fn arb_global_setter_echo() -> impl Strategy<Value = (Command, Response)> {
    prop_oneof![
        prop_oneof![
            Just(AmHighCut::Hz3000),
            Just(AmHighCut::Hz4500),
            Just(AmHighCut::Hz6000),
            Just(AmHighCut::Hz7500)
        ]
        .prop_map(|cut| (Command::SetAmHighCut { cut }, Response::AmHighCut(cut))),
        (arb_band(), arb_band()).prop_map(|(control, ptt)| {
            let roles = BandControl { control, ptt };
            (
                Command::SetBandControl { roles },
                Response::BandControl(roles),
            )
        }),
        prop_oneof![Just(BandDisplay::Dual), Just(BandDisplay::Single)].prop_map(|display| (
            Command::SetBandDisplay { display },
            Response::BandDisplay(display),
        )),
        arb_dstar_slot()
            .prop_map(|slot| (Command::SetDstarSlot { slot }, Response::DstarSlot(slot))),
        (0..=BacklightControl::MAX)
            .prop_filter_map("backlight control", |value| BacklightControl::new(value)
                .ok())
            .prop_map(|control| (
                Command::SetBacklightControl { control },
                Response::BacklightControl(control),
            )),
        (0..=MyPositionSelection::MAX)
            .prop_filter_map("position selection", |value| {
                MyPositionSelection::new(value).ok()
            })
            .prop_map(|selection| (
                Command::SetMyPositionSelection { selection },
                Response::MyPositionSelection(selection),
            )),
        prop_oneof![Just(PacketDataRate::Bps1200), Just(PacketDataRate::Bps9600)].prop_map(
            |rate| (
                Command::SetPacketDataRate { rate },
                Response::PacketDataRate(rate),
            )
        ),
        prop_oneof![
            Just(BeaconMethod::Manual),
            Just(BeaconMethod::Ptt),
            Just(BeaconMethod::Auto),
            Just(BeaconMethod::SmartBeaconing)
        ]
        .prop_map(|method| (
            Command::SetBeaconMethod { method },
            Response::BeaconMethod(method),
        )),
        (0..=VoxDelay::MAX)
            .prop_filter_map("VOX delay", |value| VoxDelay::new(value).ok())
            .prop_map(|delay| (Command::SetVoxDelay { delay }, Response::VoxDelay(delay))),
        (0..=VoxGain::MAX)
            .prop_filter_map("VOX gain", |value| VoxGain::new(value).ok())
            .prop_map(|gain| (Command::SetVoxGain { gain }, Response::VoxGain(gain))),
        any::<bool>().prop_map(|enabled| (
            Command::SetBluetooth { enabled },
            Response::Bluetooth { enabled },
        )),
    ]
}

fn arb_structured_setter_echo() -> impl Strategy<Value = (Command, Response)> {
    prop_oneof![
        arb_dstar_entry().prop_map(|entry| (
            Command::SetDstarCallsign {
                entry: entry.clone()
            },
            Response::DstarCallsign(entry),
        )),
        (any::<bool>(), any::<bool>()).prop_map(|(gps_enabled, pc_output)| {
            let settings = GpsSettings {
                gps_enabled,
                pc_output,
            };
            (
                Command::SetGpsSettings { settings },
                Response::GpsSettings(settings),
            )
        }),
        any::<[bool; 6]>().prop_map(|flags| {
            let sentences = NmeaSentences::from_flags(flags);
            (
                Command::SetGpsSentences { sentences },
                Response::GpsSentences(sentences),
            )
        }),
        "[A-Z0-9]{1,6}(-([1-9]|1[0-5]))?"
            .prop_filter_map("APRS callsign", |text| AprsCallsign::new(&text).ok())
            .prop_map(|callsign| (
                Command::SetAprsCallsign {
                    callsign: callsign.clone()
                },
                Response::AprsCallsign(callsign),
            )),
    ]
}

fn arb_setter_echo() -> impl Strategy<Value = (Command, Response)> {
    prop_oneof![
        arb_band_setter_echo(),
        arb_global_setter_echo(),
        arb_structured_setter_echo(),
    ]
}

// ============================================================================
// MCP framing and registry strategies
// ============================================================================

/// One validated transfer page anywhere inside the image.
fn arb_page() -> impl Strategy<Value = Page> {
    (0u32..IMAGE_LENGTH_U32, 1usize..=256)
        .prop_filter_map("a page inside the image", |(address, len)| {
            Page::new(Address::new(address).ok()?, len).ok()
        })
}

fn arb_header_command() -> impl Strategy<Value = HeaderCommand> {
    prop_oneof![
        Just(HeaderCommand::Read),
        Just(HeaderCommand::Write),
        Just(HeaderCommand::Fill),
    ]
}

/// One masked bit claim at an offset inside a page of `len` bytes.
fn arb_byte_patch(len: usize) -> impl Strategy<Value = BytePatch> {
    (0..len, 1u8..=u8::MAX, any::<u8>())
        .prop_filter_map("a masked claim inside the page", |(offset, mask, bits)| {
            BytePatch::new(u8::try_from(offset).ok()?, mask, bits & mask).ok()
        })
}

/// A page, one to seven claims inside it, and a random buffer of its length.
fn arb_page_patch_case() -> impl Strategy<Value = (Page, Vec<BytePatch>, Vec<u8>)> {
    arb_page().prop_flat_map(|page| {
        (
            Just(page),
            vec(arb_byte_patch(page.len()), 1..8),
            vec(any::<u8>(), page.len()),
        )
    })
}

/// One writable value inside `field`'s registered domain: a listed option or
/// allowed value when the registry lists them, otherwise the codec's range.
fn arb_field_value(field: &'static MenuField) -> BoxedStrategy<DecodedFieldValue> {
    if !field.options.is_empty() {
        let raws: Vec<u64> = field.options.iter().map(|option| option.raw).collect();
        return proptest::sample::select(raws)
            .prop_map(DecodedFieldValue::Unsigned)
            .boxed();
    }
    if !field.allowed_values.is_empty() {
        return proptest::sample::select(field.allowed_values.to_vec())
            .prop_map(DecodedFieldValue::Unsigned)
            .boxed();
    }
    match field.descriptor.codec {
        FieldCodec::Byte { min, max } | FieldCodec::BitField { min, max, .. } => (u64::from(min)
            ..=u64::from(max))
            .prop_map(DecodedFieldValue::Unsigned)
            .boxed(),
        FieldCodec::Unsigned { min, max, .. } => {
            (min..=max).prop_map(DecodedFieldValue::Unsigned).boxed()
        }
        FieldCodec::Signed { min, max, .. } => {
            (min..=max).prop_map(DecodedFieldValue::Signed).boxed()
        }
        FieldCodec::Bool | FieldCodec::BitBool { .. } => {
            any::<bool>().prop_map(DecodedFieldValue::Bool).boxed()
        }
        FieldCodec::FixedString { len, .. } => vec(
            prop_oneof![
                proptest::char::range('A', 'Z'),
                proptest::char::range('0', '9'),
                Just(' '),
            ],
            1..=len,
        )
        .prop_map(|text| DecodedFieldValue::Text(text.into_iter().collect()))
        .boxed(),
        FieldCodec::Bytes { len } => vec(any::<u8>(), len)
            .prop_map(DecodedFieldValue::Bytes)
            .boxed(),
    }
}

/// A scalar, untransformed registry field, its slot when it is per-slot, and
/// one writable value.
fn arb_registry_write()
-> impl Strategy<Value = (&'static MenuField, Option<SlotIndex>, DecodedFieldValue)> {
    (0..MCP_D750_MENU_FIELDS.len(), 0u8..SLOT_COUNT)
        .prop_filter_map("a scalar, untransformed registry field", |(index, slot)| {
            let field = MCP_D750_MENU_FIELDS.get(index)?;
            if field.is_blob || field.storage_transform.is_some() {
                return None;
            }
            let slot = if field.descriptor.is_per_slot() {
                Some(SlotIndex::new(slot).ok()?)
            } else {
                None
            };
            Some((field, slot))
        })
        .prop_flat_map(|(field, slot)| {
            arb_field_value(field).prop_map(move |value| (field, slot, value))
        })
}

/// One supported text setting, a scope of its kind, and text inside its
/// storage width made of uppercase letters, digits and spaces.
fn arb_text_setting_write() -> impl Strategy<Value = (TextSetting, TextScope, String)> {
    (0..TextSetting::all().len(), 0u8..SLOT_COUNT)
        .prop_filter_map("a registered text setting", |(index, slot)| {
            let setting = *TextSetting::all().get(index)?;
            let metadata = setting.metadata().ok()?;
            let scope = match metadata.scope {
                TextScopeKind::Global => TextScope::Global,
                TextScopeKind::PerSlot => TextScope::Slot(SlotIndex::new(slot).ok()?),
            };
            Some((setting, scope, metadata.max_bytes))
        })
        .prop_flat_map(|(setting, scope, max_bytes)| {
            vec(
                prop_oneof![
                    proptest::char::range('A', 'Z'),
                    proptest::char::range('0', '9'),
                    Just(' '),
                ],
                1..=max_bytes,
            )
            .prop_map(move |text| (setting, scope, text.into_iter().collect()))
        })
}

/// One value inside `field`'s ordinary domain: the supplemental selector
/// domains and the MY callsign text policy narrow the codec's range.
fn arb_ordinary_value(field: &'static MenuField) -> BoxedStrategy<DecodedFieldValue> {
    let name = field.descriptor.name;
    if name == "dv.MyCallsignSelectDvGateway" || name == "dv.MyDvMessageSelect" {
        return (0_u64..=5).prop_map(DecodedFieldValue::Unsigned).boxed();
    }
    if name.starts_with("radio.GroupLink") {
        let links: Vec<u64> = (0_u64..=29).chain(std::iter::once(255)).collect();
        return proptest::sample::select(links)
            .prop_map(DecodedFieldValue::Unsigned)
            .boxed();
    }
    if name.starts_with("dv.MyCallsignDvGatewayList[") && name.ends_with("].MyCallsignDvGateway") {
        return vec(
            prop_oneof![
                proptest::char::range('A', 'Z'),
                proptest::char::range('0', '9'),
            ],
            1..=6,
        )
        .prop_map(|text| DecodedFieldValue::Text(text.into_iter().collect()))
        .boxed();
    }
    arb_field_value(field)
}

/// An ordinary, untransformed registry field, its slot when it is per-slot,
/// one value inside its ordinary domain, and that value as the text a
/// `MenuAssignment` parses.
fn arb_ordinary_assignment() -> impl Strategy<
    Value = (
        &'static MenuField,
        Option<SlotIndex>,
        DecodedFieldValue,
        String,
    ),
> {
    (0..MCP_D750_MENU_FIELDS.len(), 0u8..SLOT_COUNT)
        .prop_filter_map("an ordinary registry field", |(index, slot)| {
            let field = MCP_D750_MENU_FIELDS.get(index)?;
            if field.write_policy() != MenuWritePolicy::Ordinary
                || field.storage_transform.is_some()
            {
                return None;
            }
            let slot = if field.descriptor.is_per_slot() {
                Some(SlotIndex::new(slot).ok()?)
            } else {
                None
            };
            Some((field, slot))
        })
        .prop_flat_map(|(field, slot)| {
            arb_ordinary_value(field).prop_filter_map("a scalar value", move |value| {
                let text = match &value {
                    DecodedFieldValue::Unsigned(raw) => raw.to_string(),
                    DecodedFieldValue::Signed(raw) => raw.to_string(),
                    DecodedFieldValue::Bool(flag) => flag.to_string(),
                    DecodedFieldValue::Text(text) => text.clone(),
                    DecodedFieldValue::Bytes(_) => return None,
                };
                Some((field, slot, value, text))
            })
        })
}

/// The identity every menu plan is pinned to.
fn pinned_identity() -> Result<Identity, TestCaseError> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02").map_err(to_test_err)?,
        radio_type: RadioType::new("K,2,1").map_err(to_test_err)?,
    })
}

/// The guard pages of a PM Off, Gateway Off capture plus `target`, with every
/// other byte set to `seed`: format byte 2 zero, PM selector zero at control
/// page offset 9, Gateway Off at the PM Off Gateway page's first byte.
fn plan_pages(target: Page, seed: u8) -> Result<Vec<(Page, Vec<u8>)>, TestCaseError> {
    let mut pages = std::collections::BTreeMap::new();
    let format = Page::new(Address::new(8).map_err(to_test_err)?, 40).map_err(to_test_err)?;
    let control =
        Page::new(Address::new(0x4F000).map_err(to_test_err)?, 256).map_err(to_test_err)?;
    let gateway =
        Page::new(Address::new(0x51000).map_err(to_test_err)?, 256).map_err(to_test_err)?;
    for page in [format, control, gateway, target] {
        let _previous = pages.insert(page.address().as_u32(), (page, vec![seed; page.len()]));
    }
    for (address, offset) in [(8_u32, 2_usize), (0x4F000, 9), (0x51000, 0)] {
        let (_, bytes) = pages
            .get_mut(&address)
            .ok_or_else(|| to_test_err("guard page"))?;
        *bytes
            .get_mut(offset)
            .ok_or_else(|| to_test_err("guard byte"))? = 0;
    }
    Ok(pages.into_values().collect())
}

// ============================================================================
// Properties
// ============================================================================

proptest! {
    #[test]
    fn mcp_header_round_trips_and_encodes_length_256_as_zero(
        (command, page) in (arb_header_command(), arb_page()),
    ) {
        let header = Header::new(command, page);
        let bytes = header.encode();
        prop_assert_eq!(Header::decode(&bytes).map_err(to_test_err)?, header);
        let [command_byte, high, middle, low, len] = bytes;
        prop_assert_eq!(command_byte, command.as_byte());
        prop_assert_eq!(u32::from_be_bytes([0, high, middle, low]), page.address().as_u32());
        prop_assert_eq!(usize::from(len), page.len() % 256);
        prop_assert_eq!(read_request(page), Header::new(HeaderCommand::Read, page).encode());
        prop_assert_eq!(write_request(page), Header::new(HeaderCommand::Write, page).encode());
    }

    #[test]
    fn mcp_header_decode_admits_exactly_the_in_image_pages(bytes in any::<[u8; 5]>()) {
        let [command, high, middle, low, len] = bytes;
        let address = u32::from_be_bytes([0, high, middle, low]);
        let length = if len == 0 { 256 } else { usize::from(len) };
        let inside = address < IMAGE_LENGTH_U32
            && usize::try_from(address).map_err(to_test_err)? + length <= IMAGE_LENGTH;
        let known = matches!(command, b'R' | b'W' | b'Z');
        prop_assert_eq!(Header::decode(&bytes).is_ok(), known && inside);
    }

    #[test]
    fn page_patch_admits_disjoint_claims_and_changes_only_their_bits(
        (page, claims, data) in arb_page_patch_case(),
    ) {
        let overlapping = claims.iter().enumerate().any(|(index, claim)| {
            claims.iter().skip(index + 1).any(|other| {
                other.offset() == claim.offset() && other.mask() & claim.mask() != 0
            })
        });
        let patch = match PagePatch::new(page, claims.clone()) {
            Ok(patch) => {
                prop_assert!(!overlapping, "overlapping claims must be rejected");
                patch
            }
            Err(error) => {
                prop_assert!(overlapping, "disjoint claims must be accepted: {error:?}");
                return Ok(());
            }
        };
        prop_assert!(
            patch
                .bytes()
                .iter()
                .copied()
                .map(BytePatch::offset)
                .is_sorted_by(|left, right| left < right),
            "claims are coalesced in ascending offset order"
        );
        let mut written = data.clone();
        patch.apply(&mut written).map_err(to_test_err)?;
        prop_assert!(patch.is_applied(&written).map_err(to_test_err)?);
        for (offset, (before, after)) in data.iter().zip(&written).enumerate() {
            let mask = claims
                .iter()
                .filter(|claim| usize::from(claim.offset()) == offset)
                .fold(0_u8, |mask, claim| mask | claim.mask());
            let value = claims
                .iter()
                .filter(|claim| usize::from(claim.offset()) == offset)
                .fold(0_u8, |value, claim| value | claim.value());
            prop_assert_eq!(
                after & !mask,
                before & !mask,
                "byte {} outside the claimed bits changed",
                offset
            );
            prop_assert_eq!(
                after & mask,
                value,
                "byte {} inside the claimed bits differs",
                offset
            );
        }
        let mut longer = written.clone();
        longer.push(0);
        prop_assert!(patch.apply(&mut longer).is_err(), "a longer buffer is rejected");
        let shorter = data
            .get(..page.len() - 1)
            .ok_or_else(|| to_test_err("a buffer one byte short"))?;
        prop_assert!(patch.is_applied(shorter).is_err(), "a shorter buffer is rejected");
    }

    #[test]
    fn registry_field_values_round_trip_through_encode_apply_and_read(
        (field, slot, value) in arb_registry_write(),
        seed in any::<u8>(),
    ) {
        let descriptor = &field.descriptor;
        let start = descriptor.address(slot).map_err(to_test_err)?.as_usize();
        let len = descriptor.codec.encoded_len();
        let encoded = descriptor.encode(value.as_field_value()).map_err(to_test_err)?;
        prop_assert!(!encoded.is_empty(), "{} encodes at least one byte", descriptor.name);
        let mut image = vec![seed; start + len];
        let mut claimed = vec![0_u8; len];
        for byte in &encoded {
            let owned = claimed.get_mut(byte.offset()).ok_or_else(|| {
                to_test_err(format!(
                    "{} claims offset {} beyond its {len} bytes",
                    descriptor.name,
                    byte.offset()
                ))
            })?;
            prop_assert_eq!(
                *owned & byte.mask(),
                0,
                "{} claims a bit twice at offset {}",
                descriptor.name,
                byte.offset()
            );
            *owned |= byte.mask();
            let stored = image
                .get_mut(start + byte.offset())
                .ok_or_else(|| to_test_err("the field inside the buffer"))?;
            *stored = (*stored & !byte.mask()) | byte.value();
        }
        for (offset, mask) in claimed.iter().enumerate() {
            let stored = image
                .get(start + offset)
                .ok_or_else(|| to_test_err("the field inside the buffer"))?;
            prop_assert_eq!(
                stored & !mask,
                seed & !mask,
                "{} changed unclaimed bits at offset {}",
                descriptor.name,
                offset
            );
        }
        prop_assert_eq!(descriptor.read(&image, slot).map_err(to_test_err)?, value);
    }

    #[test]
    fn patch_planner_admits_exactly_the_writable_claims_and_repeats_them_idempotently(
        (field, slot, value) in arb_registry_write(),
        seed in any::<u8>(),
    ) {
        let descriptor = &field.descriptor;
        let start = descriptor.address(slot).map_err(to_test_err)?;
        let claims = descriptor.encode(value.as_field_value()).map_err(to_test_err)?;
        let writable = claims.iter().all(|byte| {
            u32::try_from(byte.offset())
                .ok()
                .and_then(|offset| start.as_u32().checked_add(offset))
                .and_then(|address| Address::new(address).ok())
                .and_then(writable_page_for)
                .is_some()
        });
        let mut planner = PatchPlanner::new();
        match planner.set(descriptor, slot, value.as_field_value()) {
            Ok(_accepted) => {
                prop_assert!(
                    writable,
                    "{} was planned although a claim lies outside the writable regions",
                    descriptor.name
                );
            }
            Err(SchemaError::NotWritable { .. }) => {
                prop_assert!(
                    !writable,
                    "{} was refused although every claim lies inside the writable regions",
                    descriptor.name
                );
                return Ok(());
            }
            Err(error) => return Err(to_test_err(error)),
        }
        let _repeated = planner
            .set(descriptor, slot, value.as_field_value())
            .map_err(to_test_err)?;
        let set = planner.finish().map_err(to_test_err)?;
        prop_assert!(!set.is_empty(), "{} plans at least one page", descriptor.name);
        let planned: usize = set.pages().iter().map(|page| page.bytes().len()).sum();
        prop_assert_eq!(planned, claims.len(), "a repeated equal assignment adds no claim");
        let mut image = vec![seed; IMAGE_LENGTH];
        set.apply_to_image(&mut image).map_err(to_test_err)?;
        prop_assert_eq!(descriptor.read(&image, slot).map_err(to_test_err)?, value);
    }

    #[test]
    fn text_previews_plan_exactly_the_text_that_reads_back(
        (setting, scope, text) in arb_text_setting_write(),
    ) {
        let firmware = FirmwareIdentity::new("1.02").map_err(to_test_err)?;
        let image = MemoryImage::from_bytes(vec![0; IMAGE_LENGTH]).map_err(to_test_err)?;
        let view = TextImage::interpret_unqualified(&image, &firmware);
        prop_assert_eq!(view.read(setting, scope).map_err(to_test_err)?, "");
        let preview = view.preview(setting, scope, &text).map_err(to_test_err)?;
        prop_assert_eq!(preview.setting(), setting);
        prop_assert_eq!(preview.scope(), scope);
        prop_assert_eq!(preview.before(), "");
        prop_assert_eq!(preview.after(), text.as_str());
        prop_assert_eq!(preview.patches().len(), 1, "one text field lies on one page");
        let other = match scope {
            TextScope::Global => TextScope::Slot(SlotIndex::new(0).map_err(to_test_err)?),
            TextScope::Slot(_) => TextScope::Global,
        };
        prop_assert!(
            view.preview(setting, other, &text).is_err(),
            "a scope of the wrong kind is rejected"
        );
        let mut bytes = image.into_bytes();
        preview.patches().apply_to_image(&mut bytes).map_err(to_test_err)?;
        let written = MemoryImage::from_bytes(bytes).map_err(to_test_err)?;
        let written = TextImage::interpret_unqualified(&written, &firmware);
        prop_assert_eq!(written.read(setting, scope).map_err(to_test_err)?, text);
    }

    #[test]
    fn menu_plans_change_only_the_assigned_bytes_and_refuse_broken_guards(
        (field, slot, value, text) in arb_ordinary_assignment(),
        seed in any::<u8>(),
    ) {
        let descriptor = &field.descriptor;
        let start = descriptor.address(slot).map_err(to_test_err)?;
        let target = writable_page_for(start).ok_or_else(|| to_test_err("target page"))?;
        let pages = plan_pages(target, seed)?;
        let identity = pinned_identity()?;
        let snapshot = MenuFieldSnapshot::from_pages(pages.clone()).map_err(to_test_err)?;
        let assignment = MenuAssignment::new(descriptor.name, slot, &text).map_err(to_test_err)?;
        let plan =
            MenuUpdatePlan::new(&identity, &snapshot, vec![assignment]).map_err(to_test_err)?;
        let planned: Vec<u32> = plan
            .replacements()
            .iter()
            .map(|replacement| replacement.page().address().as_u32())
            .collect();
        let captured: Vec<u32> = pages.iter().map(|(page, _)| page.address().as_u32()).collect();
        prop_assert_eq!(planned, captured, "the plan covers the guards and the target, in address order");
        let claims = descriptor.encode(value.as_field_value()).map_err(to_test_err)?;
        let base = start.as_usize() - target.address().as_usize();
        for replacement in plan.replacements() {
            let page = replacement.page();
            let before = pages
                .iter()
                .find(|(candidate, _)| *candidate == page)
                .map(|(_, bytes)| bytes.as_slice())
                .ok_or_else(|| to_test_err("captured page"))?;
            prop_assert_eq!(replacement.expected(), before, "the expected image is the captured page");
            if page != target {
                prop_assert!(replacement.is_noop(), "a guard page is compared, not written");
                continue;
            }
            for (offset, (old, new)) in before.iter().zip(replacement.replacement()).enumerate() {
                let mask = claims
                    .iter()
                    .filter(|claim| base + claim.offset() == offset)
                    .fold(0_u8, |mask, claim| mask | claim.mask());
                prop_assert_eq!(new & !mask, old & !mask, "byte {} outside the field changed", offset);
            }
            prop_assert_eq!(replacement.is_noop(), before == replacement.replacement());
            let mut image = vec![0_u8; target.address().as_usize() + target.len()];
            image
                .get_mut(target.address().as_usize()..)
                .ok_or_else(|| to_test_err("image tail"))?
                .copy_from_slice(replacement.replacement());
            prop_assert_eq!(&descriptor.read(&image, slot).map_err(to_test_err)?, &value);
        }
        for (address, offset, broken) in [(8_u32, 2_usize, 1_u8), (0x4F000, 9, 7), (0x51000, 0, 2)] {
            let mut pages = pages.clone();
            let (_, bytes) = pages
                .iter_mut()
                .find(|(page, _)| page.address().as_u32() == address)
                .ok_or_else(|| to_test_err("guard page"))?;
            *bytes.get_mut(offset).ok_or_else(|| to_test_err("guard byte"))? = broken;
            let snapshot = MenuFieldSnapshot::from_pages(pages).map_err(to_test_err)?;
            let assignment = MenuAssignment::new(descriptor.name, slot, &text).map_err(to_test_err)?;
            prop_assert!(
                matches!(
                    MenuUpdatePlan::new(&identity, &snapshot, vec![assignment]),
                    Err(MenuUpdateError::MemoryFormat { .. }
                        | MenuUpdateError::PmSelection { .. }
                        | MenuUpdateError::GatewayMode { .. })
                ),
                "guard byte {} of page {} must refuse the plan",
                offset,
                address
            );
        }
    }

    #[test]
    fn configuration_diffs_report_exactly_the_changed_bytes(
        seed in any::<u8>(),
        mutations in vec((any::<u16>(), any::<u8>(), 1_u8..=u8::MAX), 0..8),
    ) {
        let schedule: Vec<Page> = menu_regions().into_iter().flat_map(Region::pages).collect();
        prop_assert_eq!(schedule.len(), 1_138, "the standard schedule has 1,138 pages");
        let before: Vec<Vec<u8>> = schedule.iter().map(|page| vec![seed; page.len()]).collect();
        let mut after = before.clone();
        for (index, offset, flip) in &mutations {
            let index = usize::from(*index) % schedule.len();
            let page = schedule.get(index).ok_or_else(|| to_test_err("schedule page"))?;
            let offset = usize::from(*offset) % page.len();
            let byte = after
                .get_mut(index)
                .and_then(|bytes| bytes.get_mut(offset))
                .ok_or_else(|| to_test_err("page byte"))?;
            *byte ^= flip;
        }
        let mut expected = Vec::new();
        for ((page, old), new) in schedule.iter().zip(&before).zip(&after) {
            for (offset, (old, new)) in old.iter().zip(new).enumerate() {
                if old != new {
                    let address = page.address().as_u32()
                        + u32::try_from(offset).map_err(to_test_err)?;
                    expected.push((address, *old, *new));
                }
            }
        }
        let identity = pinned_identity()?;
        let before_capture = StandardConfiguration::new(
            &identity,
            schedule.iter().copied().zip(before.iter().map(Vec::as_slice)),
        )
        .map_err(to_test_err)?;
        let after_capture = StandardConfiguration::new(
            &identity,
            schedule.iter().copied().zip(after.iter().map(Vec::as_slice)),
        )
        .map_err(to_test_err)?;
        let diff = StandardConfigurationDiff::between(&before_capture, &after_capture)
            .map_err(to_test_err)?;
        prop_assert_eq!(diff.compared_pages(), 1_138);
        prop_assert_eq!(diff.compared_bytes(), 289_962, "every standard byte is compared");
        let actual: Vec<(u32, u8, u8)> = diff
            .pages()
            .iter()
            .flat_map(|page| {
                page.changes()
                    .iter()
                    .map(|change| (change.address().as_u32(), change.before(), change.after()))
            })
            .collect();
        prop_assert_eq!(actual, expected, "every changed byte and nothing else, in address order");
        prop_assert_eq!(diff.changed_bytes(), diff.pages().iter().map(|page| page.changes().len()).sum::<usize>());
        prop_assert!(diff.pages().iter().all(|page| !page.changes().is_empty()), "unchanged pages are not retained");
        let unchanged = StandardConfigurationDiff::between(&before_capture, &before_capture)
            .map_err(to_test_err)?;
        prop_assert_eq!(unchanged.changed_bytes(), 0);
        prop_assert!(unchanged.pages().is_empty());
        let other = Identity {
            firmware: FirmwareIdentity::new("1.03").map_err(to_test_err)?,
            ..identity.clone()
        };
        let foreign = StandardConfiguration::new(
            &other,
            schedule.iter().copied().zip(after.iter().map(Vec::as_slice)),
        )
        .map_err(to_test_err)?;
        prop_assert!(
            matches!(
                StandardConfigurationDiff::between(&before_capture, &foreign),
                Err(ConfigurationError::IdentityMismatch { .. })
            ),
            "a differing identity is refused before any byte is compared"
        );
    }

    #[test]
    fn cat_line_parsing_never_panics(bytes in vec(any::<u8>(), 0..48)) {
        let _response = parse_line(&bytes);
    }

    #[test]
    fn frequency_wire_field_round_trips(frequency in arb_frequency()) {
        let wire = frequency.to_wire_string();
        prop_assert_eq!(wire.len(), WIRE_DIGITS);
        prop_assert_eq!(Frequency::from_wire_str(&wire).map_err(to_test_err)?, frequency);
    }

    #[test]
    fn frequency_outside_the_receiver_range_is_rejected(hz in any::<u32>()) {
        let inside = (Frequency::MIN_HZ..=Frequency::MAX_HZ).contains(&hz);
        prop_assert_eq!(Frequency::new(hz).is_ok(), inside);
    }

    #[test]
    fn megahertz_text_is_exact_integer_arithmetic(
        whole in 100u32..=530,
        fraction in 0u32..1_000_000,
        digits in 0usize..=6,
    ) {
        let scale = 10u32.pow(u32::try_from(6 - digits).map_err(to_test_err)?);
        let fraction = fraction / scale;
        let text = if digits == 0 {
            whole.to_string()
        } else {
            format!("{whole}.{fraction:0digits$}")
        };
        let hz = whole * 1_000_000 + fraction * scale;
        prop_assert_eq!(Frequency::from_mhz_str(&text).ok(), Frequency::new(hz).ok());
    }

    #[test]
    fn transmit_wire_field_selects_the_range_that_contains_it(transmit in arb_transmit()) {
        let wire = transmit.to_wire_string();
        prop_assert_eq!(wire.len(), WIRE_DIGITS);
        prop_assert_eq!(TransmitSetting::from_wire_str(&wire).map_err(to_test_err)?, transmit);
    }

    #[test]
    fn step_wire_digit_round_trips(step in arb_step()) {
        let text = step.wire_char().to_string();
        prop_assert_eq!(StepSize::from_wire_str(&text).map_err(to_test_err)?, step);
        prop_assert_eq!(StepSize::try_from(step.as_raw()).map_err(to_test_err)?, step);
    }

    #[test]
    fn stored_channel_record_round_trips(channel in arb_stored_channel()) {
        let bytes = channel.to_bytes();
        let decoded = StoredChannel::from_bytes(&bytes).map_err(to_test_err)?;
        prop_assert_eq!(&decoded, &channel);
        prop_assert_eq!(decoded.to_bytes(), bytes);
    }

    #[test]
    fn any_decodable_channel_record_re_encodes_to_the_same_bytes(
        bytes in vec(any::<u8>(), 40),
    ) {
        if let Ok(channel) = StoredChannel::from_bytes(&bytes) {
            let encoded = channel.to_bytes();
            prop_assert_eq!(encoded.as_slice(), bytes.as_slice());
        }
    }

    #[test]
    fn any_decodable_flag_record_re_encodes_to_the_same_bytes(bytes in vec(any::<u8>(), 4)) {
        if let Ok(flag) = StoredChannelFlag::from_bytes(&bytes) {
            let encoded = flag.to_bytes();
            prop_assert_eq!(encoded.as_slice(), bytes.as_slice());
        }
    }

    #[test]
    fn setter_echo_parses_to_the_command_values((command, expected) in arb_setter_echo()) {
        let response = parse_line(&command.encode()).map_err(to_test_err)?;
        prop_assert!(
            command.correlates(&response),
            "{command:?} must correlate with {response:?}"
        );
        prop_assert_eq!(response, expected);
    }

    #[test]
    fn dstar_callsign_echo_carries_the_entry_verbatim(entry in arb_dstar_entry()) {
        let command = Command::SetDstarCallsign {
            entry: entry.clone(),
        };
        let response = parse_line(&command.encode()).map_err(to_test_err)?;
        prop_assert_eq!(response, Response::DstarCallsign(entry));
    }

    #[test]
    fn dstar_callsign_entry_rejects_the_field_separator(
        slot in arb_dstar_slot(),
        callsign in "[ -~]{0,3},[ -~]{0,3}",
        memo in "[ -~]{0,1},[ -~]{0,1}",
    ) {
        prop_assert!(DstarCallsignEntry::new(slot, &callsign, "").is_err(), "{callsign:?}");
        prop_assert!(DstarCallsignEntry::new(slot, "", &memo).is_err(), "{memo:?}");
    }

    #[test]
    fn aprs_callsign_grammar_is_accepted_verbatim(text in "[A-Z0-9]{1,6}(-([1-9]|1[0-5]))?") {
        let callsign = AprsCallsign::new(&text).map_err(to_test_err)?;
        prop_assert_eq!(callsign.as_str(), text.as_str());
    }

    #[test]
    fn aprs_callsign_outside_the_grammar_is_rejected(
        text in prop_oneof![
            "[A-Z0-9]{7,9}",
            "[a-z][A-Z0-9]{0,5}",
            "[A-Z0-9]{1,6}-(0|1[6-9]|[2-9][0-9])",
            "[A-Z0-9]{1,6}-",
        ],
    ) {
        prop_assert!(AprsCallsign::new(&text).is_err(), "{text:?}");
    }

    #[test]
    fn channel_name_text_is_nul_padded_to_the_field(text in "[ -~]{1,16}") {
        let name = ChannelNameText::new(&text).map_err(to_test_err)?;
        prop_assert_eq!(name.as_str(), text.as_str());
        let field = name.to_field();
        prop_assert_eq!(field.len(), CHANNEL_NAME_SIZE);
        let (prefix, padding) = field.split_at(text.len());
        prop_assert_eq!(prefix, text.as_bytes());
        prop_assert!(padding.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn channel_name_text_outside_the_field_is_rejected(
        text in prop_oneof!["[ -~]{17,32}", "[ -~]{0,15}[\\x00-\\x1f\\x7f][ -~]{0,15}"],
    ) {
        prop_assert!(ChannelNameText::new(&text).is_err(), "{text:?}");
    }

    #[test]
    fn page_is_valid_exactly_inside_the_image(
        address in 0u32..IMAGE_LENGTH_U32,
        len in 0usize..=512,
    ) {
        let start = Address::new(address).map_err(to_test_err)?;
        let inside = (1..=256).contains(&len)
            && usize::try_from(address).map_err(to_test_err)? + len <= IMAGE_LENGTH;
        prop_assert_eq!(Page::new(start, len).is_ok(), inside);
    }

    #[test]
    fn address_region_and_slot_bounds_hold(
        address in any::<u32>(),
        start in any::<u32>(),
        end in any::<u32>(),
        slot in any::<u8>(),
    ) {
        prop_assert_eq!(Address::new(address).is_ok(), address < IMAGE_LENGTH_U32);
        prop_assert_eq!(
            Region::new(start, end).is_ok(),
            start < end && end <= IMAGE_LENGTH_U32
        );
        prop_assert_eq!(SlotIndex::new(slot).is_ok(), slot < SLOT_COUNT);
    }

    #[test]
    fn full_configuration_file_round_trips(seed in any::<u8>()) {
        let radio_type = RadioType::new("K,2,1").map_err(to_test_err)?;
        let header = ConfigHeader::for_mcp_d750(&radio_type).map_err(to_test_err)?;
        prop_assert_eq!(header.layout(), FileLayout::Full);
        prop_assert_eq!(
            &ConfigHeader::try_from(*header.as_bytes()).map_err(to_test_err)?,
            &header
        );
        let image: Vec<u8> = (0..FileLayout::Full.image_bytes())
            .map(|index| seed.wrapping_add(u8::try_from(index % 251).unwrap_or_default()))
            .collect();
        let config = RadioConfig::new(header, image).map_err(to_test_err)?;
        let bytes = config.to_bytes();
        prop_assert_eq!(bytes.len(), FileLayout::Full.file_size());
        prop_assert_eq!(parse_d750(&bytes).map_err(to_test_err)?, config);
    }

    #[test]
    fn short_configuration_file_retains_opaque_header_bytes(
        raw in vec(any::<u8>(), HEADER_SIZE),
        seed in any::<u8>(),
    ) {
        let mut raw: [u8; HEADER_SIZE] = raw.try_into().map_err(to_test_err)?;
        for (offset, value) in [
            (0, b"TM-D750".as_slice()),
            (8, b"V1.00".as_slice()),
            (16, b"TM-D750".as_slice()),
            (32, &[0]),
        ] {
            for (destination, source) in raw.iter_mut().skip(offset).zip(value) {
                *destination = *source;
            }
        }
        let header = ConfigHeader::try_from(raw).map_err(to_test_err)?;
        prop_assert_eq!(header.layout(), FileLayout::WithoutStartupScreen);
        prop_assert_eq!(header.as_bytes(), &raw);
        let image = vec![seed; FileLayout::WithoutStartupScreen.image_bytes()];
        let config = RadioConfig::new(header, image).map_err(to_test_err)?;
        let bytes = config.to_bytes();
        prop_assert_eq!(parse_d750(&bytes).map_err(to_test_err)?, config);
    }
}
