// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Hardware ground-truth pin for the MCP memory-image layout.
//!
//! Every other memory test in this crate builds a synthetic image,
//! writes fields into it, and reads them back. That round-trip is
//! blind to the failure mode that actually matters here: the channel
//! and settings decoders are hand-maintained BITFIELD MAPS (mode
//! field in byte 0x09, one-hot tone nibble and split/duplex bits in byte 0x0A,
//! region offsets 0x2000/0x4000/0x10000, the settings cells spanning
//! 0x1000..0x1A10 plus the D-STAR MY-callsign list at 0x1CA8). A
//! symmetric mistake (writing and reading the same wrong bit)
//! passes every round-trip test while corrupting the radio.
//!
//! This file decodes a real 500,480-byte MCP dump read off a physical
//! TH-D75 and asserts values known to be true of that radio. Nothing
//! here is re-derived from our own encoder, so a layout regression has
//! nowhere to hide.
//!
//! The fixtures are committed, so this runs in CI like any other test.

use kenwood_thd75::memory::dstar::{
    DstarCallsignListIndex, DstarMyCallsignMemo, DstarMyCallsignSlot, DstarRepeaterIndex,
    DstarRepeaterLabelError,
};
use kenwood_thd75::memory::{MemoryImage, SettingsAccess};
// `ChannelMode` is the shared 0..8 operating-mode domain. MCP/SD-card records
// pack it into byte 0x09, while FO/ME carries the same value as a text field.
use kenwood_thd75::types::settings::{
    AmHighCut, BatterySaverInterval, CwFilterWidth, DtmfToneDuration, FrontPanelPfFunction,
    LinkedVolumeLevel, PcOutputInterface, RepeaterCallKey, SsbHighCut,
    StoredFrontPanelPfAssignment, TransmitTimeout, VoiceAnnounceMode,
};
use kenwood_thd75::types::{
    AltitudeRainUnit, AutoPowerOff, BacklightControl, BeatShift, ChannelDisplayName, ChannelMode,
    DstarCallsign, DtmfPause, Language, MemoryChannelBand, MemoryGroup, MicSensitivity,
    RegularChannel, ScanResumeMethod, SpeedDistanceUnit, StepSize, StoredChannel, TemperatureUnit,
    VoiceGuideSpeed,
};

// Deps visible to this compilation unit but unused here.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Expected size of a full MCP dump (all four regions).
const DUMP_LEN: usize = 500_480;

fn load_dump() -> Result<MemoryImage, Box<dyn std::error::Error>> {
    let raw = std::fs::read("tests/fixtures/memory_dump.bin")?;
    assert_eq!(raw.len(), DUMP_LEN, "fixture is not a full MCP dump");
    Ok(MemoryImage::from_raw(raw)?)
}

fn assert_valid_stored_tone_nibble(channel: &StoredChannel, context: &str) -> TestResult {
    let bytes = channel.to_bytes();
    let tone_nibble = bytes
        .get(0x0A)
        .copied()
        .ok_or("stored channel has no byte 0x0A")?
        >> 4;
    assert!(
        matches!(tone_nibble, 0 | 1 | 2 | 4 | 8),
        "{context} has non-one-hot stored tone nibble 0x{tone_nibble:X}"
    );
    Ok(())
}

/// The retained radio image disproves the old claim that `0x4D000` is a
/// 100-byte GPS waypoint index. It starts with paired-device records: two
/// recognizable Bluetooth device names separated by binary metadata. Under
/// the former `0xFF`-means-unused heuristic, these bytes would falsely expose
/// all 100 positions as waypoints.
#[test]
fn offset_4d000_is_paired_device_data_not_a_waypoint_index() -> TestResult {
    let image = load_dump()?;
    let alleged_index = image
        .as_raw()
        .get(0x4_D000..0x4_D064)
        .ok_or("fixture is missing the 0x4D000 paired-device region")?;

    assert_eq!(
        alleged_index.get(8..20),
        Some(b"MacBook Pro\0".as_slice()),
        "first record contains a Bluetooth device name"
    );
    assert_eq!(
        alleged_index.get(0x24..0x2E),
        Some(b"B.B. Link\0".as_slice()),
        "second record contains another Bluetooth device name"
    );
    assert!(
        alleged_index.iter().all(|&byte| byte != 0xFF),
        "the disproven index heuristic would misreport 100 occupied waypoint slots"
    );
    Ok(())
}

/// The real dump parses, and every channel slot the radio marks as
/// used decodes into a valid `StoredChannel`.
///
/// Corrupt records are hard errors in the channel decoder, so a
/// layout regression that shifts a record boundary shows up here as
/// an unparseable slot rather than as silently wrong data.
///
/// The counts also pin the regular/special split: `count()` covers
/// only the regular bank (0..=999), while the flag region spans all
/// 1,200 entries; the extra used slots on this radio are its special
/// channels (call/scan-edge/weather). A regression that moved the
/// regular-bank boundary would change one count without the other.
#[test]
fn real_dump_parses_and_every_used_channel_decodes() -> TestResult {
    let image = load_dump()?;
    let channels = image.channels();

    assert_eq!(
        channels.count()?,
        140,
        "the source radio has 140 regular channels programmed (bank 0..=999)"
    );

    let mut used_regular = 0usize;
    let mut used_special = 0usize;
    for channel in RegularChannel::all() {
        if channels.is_used(channel)? {
            used_regular += 1;
            let stored = channels.stored_data(channel)?;
            let programmed = stored
                .programmed()
                .ok_or("used regular channel did not decode as programmed")?;
            assert_valid_stored_tone_nibble(programmed, &format!("regular channel {channel}"))?;
        }
    }

    let raw = image.as_raw();
    for number in 1_000usize..1_152 {
        let flag_offset = 0x2000 + number * 4;
        let marker = *raw
            .get(flag_offset)
            .ok_or("special-channel flag is outside the fixture")?;
        if marker != 0xFF {
            used_special += 1;
            let memgroup = number / 6;
            let slot = number % 6;
            let data_offset = 0x4000 + memgroup * 256 + slot * 40;
            let record = raw
                .get(data_offset..data_offset + 40)
                .ok_or("special-channel record is outside the fixture")?;
            let programmed = StoredChannel::from_bytes(record)?;
            assert_valid_stored_tone_nibble(
                &programmed,
                &format!("special channel {number} (group {memgroup}, slot {slot})"),
            )?;
        }
    }
    assert_eq!(
        used_regular,
        channels.count()?,
        "the regular-bank scan must agree with count()"
    );
    assert_eq!(
        used_special, 16,
        "the source radio has 16 special channels programmed (bank 1000..=1199)"
    );
    Ok(())
}

/// Known-true channel contents, read off the physical radio.
///
/// These are public-safety memories programmed into the source
/// handheld: frequency, name, mode, step and shift all come from the
/// hardware, not from our encoder. A wrong mode nibble, a shifted
/// frequency word, or an off-by-one in the 32-byte record stride
/// fails here.
#[test]
fn channel_zero_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let channels = image.channels();

    let ch0 = channels.get(RegularChannel::new(0)?)?;
    let ch0_programmed = ch0.programmed().ok_or("channel 0 should be programmed")?;
    assert_eq!(ch0.name().as_str(), "RCOFIRETAC", "channel 0 name");
    assert_eq!(
        ch0_programmed.receive_frequency.as_hz(),
        154_205_000,
        "channel 0 RX frequency (154.205 MHz)"
    );
    assert_eq!(
        ch0_programmed.mode,
        ChannelMode::Fm,
        "channel 0 is an FM memory"
    );
    assert_eq!(
        ch0_programmed.receive_step,
        StepSize::Hz5000,
        "channel 0 tuning step"
    );
    assert_ne!(
        ch0_programmed.mode,
        ChannelMode::Nfm,
        "channel 0 is wide FM"
    );
    assert!(ch0.is_programmed(), "channel 0 is a programmed slot");
    assert_eq!(
        ch0.flag().scan_lockout(),
        Some(false),
        "channel 0 is not locked out"
    );

    // Two more memories fix the record stride: a boundary error that
    // still produced a plausible channel 0 cannot also produce these.
    let ch1 = channels.get(RegularChannel::new(1)?)?;
    let ch1_programmed = ch1.programmed().ok_or("channel 1 should be programmed")?;
    assert_eq!(ch1.name().as_str(), "RCOEMSTAC1", "channel 1 name");
    assert_eq!(
        ch1_programmed.receive_frequency.as_hz(),
        155_220_000,
        "channel 1 RX frequency (155.220 MHz)"
    );
    assert_eq!(
        ch1.flag().to_wire_bytes(),
        [0x08, 0x00, 0x00, 0x00],
        "channel 1 retains its complete physical flag record"
    );
    assert_eq!(ch1.flag().band(), Some(MemoryChannelBand::Vhf));
    assert_eq!(ch1.flag().group(), Some(MemoryGroup::new(0)?));
    assert_eq!(ch1.flag().scan_lockout(), Some(false));

    let ch2 = channels.get(RegularChannel::new(2)?)?;
    let ch2_programmed = ch2.programmed().ok_or("channel 2 should be programmed")?;
    assert_eq!(ch2.name().as_str(), "RCOEMSTAC2", "channel 2 name");
    assert_eq!(
        ch2_programmed.receive_frequency.as_hz(),
        155_280_000,
        "channel 2 RX frequency (155.280 MHz)"
    );
    Ok(())
}

#[test]
fn channel_writer_preserves_opaque_physical_flag_bits() -> TestResult {
    let mut image = load_dump()?;
    let channel = RegularChannel::new(1)?;
    let entry = image.channels().get(channel)?;
    let exact_flag = entry.flag().to_wire_bytes();

    image.channels_mut().set(&entry)?;

    assert_eq!(exact_flag, [0x08, 0x00, 0x00, 0x00]);
    assert_eq!(
        image.channels().flag(channel)?.to_wire_bytes(),
        exact_flag,
        "rewriting a parsed channel must not erase unmodelled flag data"
    );
    Ok(())
}

/// The physical radio uses every byte of the 16-byte channel-name field.
/// A writer that reserves byte 16 for a NUL terminator corrupts this value.
#[test]
fn full_width_weather_channel_name_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let name_offset = 0x10000 + 1_101 * ChannelDisplayName::WIRE_LEN;
    let name_bytes: [u8; ChannelDisplayName::WIRE_LEN] = image
        .as_raw()
        .get(name_offset..name_offset + ChannelDisplayName::WIRE_LEN)
        .ok_or("weather channel 1 name slot must be present")?
        .try_into()?;
    let name = ChannelDisplayName::try_from_wire(name_bytes)?;

    assert_eq!(name.len(), 16, "fixture must exercise the complete field");
    assert_eq!(name.as_str(), "WX  1 Greenville");
    assert_eq!(name.to_wire_bytes(), *b"WX  1 Greenville");
    Ok(())
}

fn assert_display_and_lock_settings(settings: &SettingsAccess<'_>) -> TestResult {
    assert!(settings.key_beep()?, "key beep was enabled on the radio");
    assert_eq!(
        settings.beep_volume()?,
        LinkedVolumeLevel::VOLUME_LINK,
        "beep volume is VOL Link"
    );
    assert_eq!(
        settings.announce()?,
        VoiceAnnounceMode::Off,
        "voice announce is Off"
    );
    assert_eq!(
        settings.voice_volume()?,
        LinkedVolumeLevel::VOLUME_LINK,
        "voice announce volume is VOL Link (0)"
    );
    assert_eq!(
        settings.voice_speed()?,
        VoiceGuideSpeed::Speed1,
        "voice guidance speed is Speed 1"
    );
    assert_eq!(
        settings.backlight_control()?,
        BacklightControl::On,
        "backlight control is On"
    );
    assert_eq!(
        settings.backlight_timer()?.as_seconds(),
        13,
        "backlight timer is 13 s"
    );
    assert!(settings.key_lock()?, "key-lock checkbox was ticked");
    assert!(
        settings.frequency_lock()?,
        "frequency-lock checkbox was ticked"
    );
    assert!(!settings.volume_lock()?, "volume lock was off");
    assert!(!settings.aprs_lock_frequency()?, "APRS frequency lock off");
    assert!(!settings.aprs_lock_ptt()?, "APRS PTT lock off");
    assert!(!settings.aprs_lock_key()?, "APRS key lock off");
    Ok(())
}

fn assert_transmit_receive_and_scan_settings(settings: &SettingsAccess<'_>) -> TestResult {
    assert!(!settings.tx_inhibit()?, "TX inhibit was off");
    assert_eq!(
        settings.timeout_timer()?,
        TransmitTimeout::Seconds600,
        "TX timeout is 10 minutes"
    );
    assert_eq!(
        settings.beat_shift()?,
        BeatShift::Type1,
        "beat shift Type 1"
    );
    assert_eq!(
        settings.mic_sensitivity()?,
        MicSensitivity::Medium,
        "mic sensitivity Medium (0=High on the D75)"
    );
    assert_eq!(
        settings.ssb_high_cut()?,
        SsbHighCut::Khz2_4,
        "SSB high cut 2.4 kHz"
    );
    assert_eq!(
        settings.cw_width()?,
        CwFilterWidth::Khz1_0,
        "CW width 1.0 kHz"
    );
    assert_eq!(
        settings.am_high_cut()?,
        AmHighCut::Khz6_0,
        "AM high cut 6.0 kHz"
    );
    assert_eq!(settings.cw_pitch()?.as_hz(), 800, "CW pitch is 800 Hz");
    assert_eq!(
        settings.scan_resume()?,
        ScanResumeMethod::CarrierOperated,
        "analog scan resume Carrier"
    );
    assert_eq!(
        settings.digital_scan_resume()?,
        ScanResumeMethod::Seek,
        "digital scan resume Seek"
    );
    assert_eq!(
        settings.scan_restart_time()?.as_seconds(),
        8,
        "time restart 8 s"
    );
    assert_eq!(
        settings.scan_restart_carrier()?.as_seconds(),
        4,
        "carrier restart 4 s"
    );
    assert!(!settings.vox_enabled()?, "VOX was off");
    assert_eq!(settings.vox_gain()?.as_raw(), 4, "VOX gain");
    assert_eq!(
        settings.vox_delay()?.as_raw(),
        1,
        "VOX delay index 1 = 500 ms"
    );
    assert!(!settings.vox_tx_on_busy()?, "VOX TX-on-busy was off");
    assert_eq!(
        settings.dtmf_speed()?,
        DtmfToneDuration::Ms100,
        "DTMF speed 100 ms"
    );
    assert_eq!(
        settings.dtmf_pause_time()?,
        DtmfPause::Ms500,
        "DTMF pause 500 ms"
    );
    assert!(!settings.dtmf_tx_hold()?, "DTMF TX hold was off");
    assert!(settings.repeater_auto_offset()?, "auto offset was on");
    assert_eq!(
        settings.repeater_call_key()?,
        RepeaterCallKey::CallChannel,
        "CALL key function CALL"
    );
    assert_eq!(
        settings.pf_key1()?,
        StoredFrontPanelPfAssignment::Official(FrontPanelPfFunction::Balance),
        "PF1 assigned to Balance"
    );
    assert_eq!(
        settings.pf_key2()?,
        StoredFrontPanelPfAssignment::Official(FrontPanelPfFunction::Gps),
        "PF2 assigned to GPS"
    );
    Ok(())
}

fn assert_audio_connectivity_and_system_settings(settings: &SettingsAccess<'_>) -> TestResult {
    assert_eq!(
        settings.emr_volume_level()?.as_raw(),
        25,
        "EMR volume level 25"
    );
    assert_eq!(
        settings.auto_mute_return_time()?.as_seconds(),
        3,
        "auto mute return 3 seconds"
    );
    assert_eq!(
        settings.gps_pc_output_interface()?,
        PcOutputInterface::Bluetooth,
        "GPS PC output Bluetooth"
    );
    assert_eq!(
        settings.aprs_pc_output_interface()?,
        PcOutputInterface::Bluetooth,
        "APRS PC output Bluetooth"
    );
    assert!(settings.bluetooth()?, "Bluetooth was on");
    assert!(settings.bt_auto_connect()?, "BT auto-connect was on");
    assert_eq!(
        settings.battery_saver()?,
        BatterySaverInterval::Off,
        "battery saver Off"
    );
    assert_eq!(
        settings.auto_power_off()?,
        AutoPowerOff::Off,
        "auto power off disabled"
    );
    let units = settings.display_units()?;
    assert_eq!(
        units.speed_distance,
        SpeedDistanceUnit::MilesPerHour,
        "US units: speed in mph"
    );
    assert_eq!(
        units.altitude_rain,
        AltitudeRainUnit::FeetInch,
        "US units: altitude in feet"
    );
    assert_eq!(
        units.temperature,
        TemperatureUnit::Fahrenheit,
        "US units: temperature in F"
    );
    assert_eq!(settings.language()?, Language::English, "display language");
    assert_eq!(
        settings.power_on_message()?.as_str(),
        "",
        "no power-on message programmed"
    );
    Ok(())
}

/// Known-true settings bytes, read off the physical radio.
///
/// The settings layer is a flat offset map: every accessor is a
/// hand-written byte/bit index sourced from the MCP-D75 field
/// registry. Decoding the real dump pins each index to hardware, so
/// an accessor that reads the wrong cell (the historical failure mode
/// of this layer) cannot return the value the radio actually holds.
#[test]
fn settings_block_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let settings = image.settings();

    assert_display_and_lock_settings(&settings)?;
    assert_transmit_receive_and_scan_settings(&settings)?;
    assert_audio_connectivity_and_system_settings(&settings)?;
    Ok(())
}

/// Known-true D-STAR MY-callsign record, read off the physical radio.
///
/// The MY callsign lives in `dv.MyCallsignDvGatewayList` at 0x1CA8
/// (8-byte space-padded callsign + 4-byte memo, stride 12). The
/// source radio has its owner's callsign and a "D75A" memo in record
/// 0, with the selector pointing at that record.
#[test]
fn dstar_my_callsign_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let dstar = image.dstar();
    let slot_0 = DstarMyCallsignSlot::new(0)?;
    let slot_1 = DstarMyCallsignSlot::new(1)?;

    assert_eq!(
        dstar.my_callsign_select()?.as_raw(),
        0,
        "record 0 is active"
    );
    assert_eq!(
        dstar.my_callsign()?.as_ref().map(DstarCallsign::as_str),
        Some("KQ4NIT"),
        "MY callsign"
    );
    assert_eq!(
        dstar
            .my_callsign_record(slot_0)?
            .memo()
            .map(DstarMyCallsignMemo::as_str),
        Some("D75A"),
        "record 0 memo"
    );
    assert_eq!(
        dstar.my_callsign_record(slot_1)?.callsign(),
        None,
        "record 1 is unprogrammed"
    );
    Ok(())
}

/// The retained radio initializes every direct-callsign record to the same
/// exact 64-byte pattern. This pins the table's start, stride, and count while
/// leaving its internal fields opaque.
#[test]
fn dstar_direct_callsign_table_matches_the_radio() -> TestResult {
    let image = load_dump()?;
    let mut expected = [0; 64];
    expected
        .get_mut(56..)
        .ok_or("64-byte expected record must contain its final eight bytes")?
        .fill(0xFF);

    for raw_index in 0..DstarCallsignListIndex::COUNT {
        let index = DstarCallsignListIndex::new(raw_index)?;
        assert_eq!(
            image.dstar().callsign_list_record_bytes(index)?.as_bytes(),
            &expected,
            "unexpected initialized record at direct-callsign slot {raw_index}"
        );
    }
    Ok(())
}

/// The physical repeater directory pins both the 80-byte field layout and the
/// three-record page packing. Slot 3 starts on the next page after the
/// 16-byte page trailer; treating records as a linear array reads the wrong
/// station here.
#[test]
fn dstar_repeater_directory_matches_the_radio_across_a_page_boundary() -> TestResult {
    let image = load_dump()?;
    let dstar = image.dstar();
    let slot_0 = DstarRepeaterIndex::new(0)?;
    let slot_3 = DstarRepeaterIndex::new(3)?;
    let empty_slot = DstarRepeaterIndex::new(265)?;

    let first = dstar
        .repeater_record(slot_0)?
        .ok_or("repeater slot 0 must be occupied")?;
    assert_eq!(first.name().decode_utf8()?, "Akihabara");
    assert_eq!(first.area().decode_utf8()?, "Tokyo");
    assert_eq!(first.callsign_rpt1().as_str(), "JP1YLA A");
    assert_eq!(
        first.gateway_rpt2().map(DstarCallsign::as_str),
        Some("JP1YLA G")
    );
    assert_eq!(first.frequency().as_hz(), 434_320_000);
    assert_eq!(first.tx_offset().as_hz(), 5_000_000);

    let across_page = dstar
        .repeater_record(slot_3)?
        .ok_or("repeater slot 3 must be occupied")?;
    assert_eq!(across_page.name().decode_utf8()?, "Edogawa");
    assert_eq!(across_page.area().decode_utf8()?, "Tokyo");
    assert_eq!(across_page.callsign_rpt1().as_str(), "JP1YJK A");
    assert_eq!(across_page.frequency().as_hz(), 439_070_000);
    assert_eq!(across_page.tx_offset().as_hz(), 5_000_000);
    assert_eq!(
        dstar.repeater_record(empty_slot)?,
        None,
        "slot 265 uses the radio's initialized-empty record pattern"
    );
    assert_eq!(dstar.repeater_count()?, 1_455);
    Ok(())
}

/// The retained directory includes legacy single-byte label text. Slot 125's
/// area is `Li` + `0xE8` + `ge`; replacing that byte to force UTF-8 would make
/// an apparently friendly display string while silently changing radio data.
#[test]
fn dstar_repeater_legacy_labels_remain_lossless() -> TestResult {
    let image = load_dump()?;
    let slot = DstarRepeaterIndex::new(125)?;
    let record = image
        .dstar()
        .repeater_record(slot)?
        .ok_or("repeater slot 125 must be occupied")?;
    let area = record.area();

    assert_eq!(
        area.as_bytes().get(..5),
        Some([b'L', b'i', 0xE8, b'g', b'e'].as_slice())
    );
    assert_eq!(
        area.decode_utf8(),
        Err(DstarRepeaterLabelError::InvalidUtf8 {
            valid_up_to: 2,
            bytes: [
                b'L', b'i', 0xE8, b'g', b'e', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        })
    );
    Ok(())
}
