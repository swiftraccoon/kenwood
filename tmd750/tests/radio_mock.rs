//! Identity proof and MCP exchanges over a scripted mock transport.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{ACK, ENTER, EXIT, Header};
use kenwood_tmd750::radio::Radio;
use kenwood_tmd750::{
    Address, Band, CommandName, DvGatewayMode, Error, OperatingMode, Page, ProtocolError, Region,
    SelectableMode, TimeoutOperation,
};
use kenwood_transport::MockTransport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scripted_identity(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.00\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn scripted_live_identity(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn read_reply(page: Page, data: &[u8]) -> Vec<u8> {
    let mut reply = Header::write_request(page).to_vec();
    reply.extend_from_slice(data);
    reply
}

fn fill_reply(page: Page, fill: u8) -> Vec<u8> {
    let mut reply = Header::write_request(page).to_vec();
    if let Some(first) = reply.first_mut() {
        *first = b'Z';
    }
    reply.push(fill);
    reply
}

#[tokio::test]
async fn identify_proves_the_model_and_caches_it() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    let mut radio = Radio::new(mock);
    let identity = radio.identify().await?;
    assert_eq!(identity.firmware.as_str(), "1.00");
    assert_eq!(identity.radio_type.as_str(), "K,2,1");
    assert_eq!(radio.identity(), Some(&identity));
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn operating_modes_and_gateway_state_are_typed_reads() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 0,0\r");
    mock.expect(b"MD 1\r", b"MD 1,7\r");
    mock.expect(b"GW\r", b"GW 2\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_operating_mode(Band::A).await?, OperatingMode::Fm);
    assert_eq!(
        radio.get_operating_mode(Band::B).await?,
        OperatingMode::from(7)
    );
    assert_eq!(
        radio.get_dv_gateway_mode().await?,
        DvGatewayMode::Terminal,
        "the read-only GW query must return the observed Terminal state"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn dstar_selection_requires_matching_echo_and_readback() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"MD 0,1\r");
    mock.expect(b"MD 0\r", b"MD 0,1\r");
    let mut radio = Radio::new(mock);
    radio.enter_dstar(Band::A).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_a_mismatched_readback() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 1,1\r", b"MD 1,1\r");
    mock.expect(b"MD 1\r", b"MD 1,0\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::B, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::ReadbackMismatch {
                command: CommandName::Md,
                ref requested,
                ref observed,
            })) if requested == "DV" && observed == "FM"
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_an_unqualified_target_before_writing() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::UnsupportedWriteTarget { ref actual }) if !actual.is_write_target()
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_an_unqualified_radio_type_before_writing() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,2\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::UnsupportedWriteTarget { ref actual })
                if actual.firmware.as_str() == "1.02" && actual.radio_type.as_str() == "K,2,2"
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_refuses_a_mismatched_write_echo_before_readback() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"MD 0,0\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::EchoMismatch {
                command: CommandName::Md,
                ref requested,
                ref echoed,
            })) if requested == "DV" && echoed == "FM"
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn an_out_of_range_frequency_reports_where_the_radio_tuned() -> TestResult {
    use kenwood_tmd750::types::Frequency;

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"FQ 0,0300000000\r", b"FQ 0,0410000000\r");
    let mut radio = Radio::new(mock);
    let requested = Frequency::new(300_000_000)?;
    let expected_tuned = Frequency::new(410_000_000)?;
    let result = radio.set_frequency(Band::A, requested).await;
    assert!(
        matches!(
            result,
            Err(Error::FrequencyAdjusted { band: Band::A, requested: asked, tuned })
                if asked == requested && tuned == expected_tuned
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn mode_selection_reports_a_rejected_or_unavailable_write() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"N\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::NotAvailable {
                command: CommandName::Md
            }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"?\r");
    let mut radio = Radio::new(mock);
    let result = radio.set_operating_mode(Band::A, SelectableMode::Dv).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::Rejected {
                command: CommandName::Md
            }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn operating_mode_reads_discard_the_other_bands_reply() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 1,1\rMD 0,0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_operating_mode(Band::A).await?, OperatingMode::Fm);
    radio.into_transport().assert_complete();

    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 1,1\r");
    mock.pend_when_empty();
    let mut radio = Radio::new(mock);
    radio.set_timeout(std::time::Duration::from_millis(20));
    let result = radio.get_operating_mode(Band::A).await;
    assert!(matches!(result, Err(Error::Timeout { .. })), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn mode_selection_discards_a_stale_readback_from_the_other_band() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"MD 0,1\r", b"MD 0,1\r");
    mock.expect(b"MD 0\r", b"MD 1,1\rMD 0,1\r");
    let mut radio = Radio::new(mock);
    radio
        .set_operating_mode(Band::A, SelectableMode::Dv)
        .await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn verified_setters_apply_the_gate_then_echo_then_readback() -> TestResult {
    use kenwood_tmd750::types::{
        BandControl, BandDisplay, Frequency, PowerLevel, SquelchLevel, StepSize, TuningMode,
    };

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"FQ 0,0145200000\r", b"FQ 0,0145200000\r");
    mock.expect(b"FQ 0\r", b"FQ 0,0145200000\r");
    mock.expect(b"PC 1,2\r", b"PC 1,2\r");
    mock.expect(b"PC 1\r", b"PC 1,2\r");
    mock.expect(b"VM 0,3\r", b"VM 0,3\r");
    mock.expect(b"VM 0\r", b"VM 0,3\r");
    mock.expect(b"SQ 0,31\r", b"SQ 0,31\r");
    mock.expect(b"SQ 0\r", b"SQ 0,31\r");
    mock.expect(b"SF 0,C\r", b"SF 0,C\r");
    mock.expect(b"SF 0\r", b"SF 0,C\r");
    mock.expect(b"BC 1,0\r", b"BC 1,0\r");
    mock.expect(b"BC\r", b"BC 1,0\r");
    mock.expect(b"DL 1\r", b"DL 1\r");
    mock.expect(b"DL\r", b"DL 1\r");
    mock.expect(b"BT 0\r", b"BT 0\r");
    mock.expect(b"BT\r", b"BT 0\r");
    let mut radio = Radio::new(mock);
    radio
        .set_frequency(Band::A, Frequency::new(145_200_000)?)
        .await?;
    radio.set_power_level(Band::B, PowerLevel::Low).await?;
    radio
        .set_tuning_mode(Band::A, TuningMode::DStarRepeater)
        .await?;
    radio.set_squelch(Band::A, SquelchLevel::new(31)?).await?;
    radio.set_step_size(Band::A, StepSize::Hz100000).await?;
    radio
        .set_band_control(BandControl {
            control: Band::B,
            ptt: Band::A,
        })
        .await?;
    radio.set_band_display(BandDisplay::Single).await?;
    radio.set_bluetooth(false).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn dstar_callsign_write_echoes_reads_back_and_clears() -> TestResult {
    use kenwood_tmd750::types::{DstarCallsignEntry, DstarCallsignText, DstarSlot};

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"DC 6,KQ4NIT,RIG\r", b"DC 6,KQ4NIT,RIG\r");
    mock.expect(b"DC 6\r", b"DC 6,KQ4NIT,RIG\r");
    mock.expect(b"DC 6,,\r", b"DC 6,,\r");
    mock.expect(b"DC 6\r", b"DC 6,,\r");
    let mut radio = Radio::new(mock);
    let slot = DstarSlot::new(6)?;
    radio
        .set_dstar_callsign(&DstarCallsignEntry::new(
            slot,
            DstarCallsignText {
                callsign: "KQ4NIT".into(),
                memo: "RIG".into(),
            },
        )?)
        .await?;
    radio.clear_dstar_callsign(slot).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn dstar_callsign_write_refuses_a_mismatched_echo() -> TestResult {
    use kenwood_tmd750::types::{DstarCallsignEntry, DstarCallsignText, DstarSlot};

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    // A truncated memo echo is not the requested entry, so the write is a
    // protocol error and no readback follows.
    mock.expect(b"DC 6,KQ4NIT,RIG\r", b"DC 6,KQ4NIT,RI\r");
    let mut radio = Radio::new(mock);
    let result = radio
        .set_dstar_callsign(&DstarCallsignEntry::new(
            DstarSlot::new(6)?,
            DstarCallsignText {
                callsign: "KQ4NIT".into(),
                memo: "RIG".into(),
            },
        )?)
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::EchoMismatch {
                command: CommandName::Dc,
                ..
            }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn aprs_callsign_reads_writes_and_reads_back() -> TestResult {
    use kenwood_tmd750::types::AprsCallsign;

    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS NOCALL\r");
    scripted_live_identity(&mut mock);
    mock.expect(b"CS KQ4NIT-9\r", b"CS KQ4NIT-9\r");
    mock.expect(b"CS\r", b"CS KQ4NIT-9\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_aprs_callsign().await?,
        AprsCallsign::new("NOCALL")?
    );
    radio
        .set_aprs_callsign(&AprsCallsign::new("KQ4NIT-9")?)
        .await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn aprs_callsign_write_refuses_a_mismatched_echo() -> TestResult {
    use kenwood_tmd750::types::AprsCallsign;

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    // The radio echoes the callsign without the SSID, so the write is not the
    // requested identity and no readback follows.
    mock.expect(b"CS KQ4NIT-9\r", b"CS KQ4NIT\r");
    let mut radio = Radio::new(mock);
    let result = radio
        .set_aprs_callsign(&AprsCallsign::new("KQ4NIT-9")?)
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::EchoMismatch {
                command: CommandName::Cs,
                ref requested,
                ref echoed,
            })) if requested == "KQ4NIT-9" && echoed == "KQ4NIT"
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn frequency_stepping_requires_the_control_band() -> TestResult {
    use kenwood_tmd750::types::Frequency;

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"BC\r", b"BC 0,0\r");
    mock.expect(b"FQ 0\r", b"FQ 0,0145190000\r");
    mock.expect(b"UP\r", b"UP\r");
    mock.expect(b"FQ 0\r", b"FQ 0,0145190000\r");
    mock.expect(b"FQ 0\r", b"FQ 0,0145195000\r");
    mock.expect(b"BC\r", b"BC 0,0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.frequency_up(Band::A).await?,
        Frequency::new(145_195_000)?,
        "a readback that still shows the old frequency is retried"
    );
    let refused = radio.frequency_down(Band::B).await;
    assert!(
        matches!(
            refused,
            Err(Error::NotControlBand {
                band: Band::B,
                control: Band::A
            })
        ),
        "{refused:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn an_acknowledged_step_that_never_applies_is_an_error() -> TestResult {
    use kenwood_tmd750::radio::STEP_READBACK_ATTEMPTS;

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"BC\r", b"BC 0,0\r");
    mock.expect(b"FQ 0\r", b"FQ 0,0145190000\r");
    mock.expect(b"DW\r", b"DW\r");
    for _ in 0..STEP_READBACK_ATTEMPTS {
        mock.expect(b"FQ 0\r", b"FQ 0,0145190000\r");
    }
    let mut radio = Radio::new(mock);
    let result = radio.frequency_down(Band::A).await;
    assert!(
        matches!(
            result,
            Err(Error::StepNotApplied {
                band: Band::A,
                step: CommandName::Dw,
                ..
            })
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn memory_channel_writes_verify_the_echo_and_the_readback() -> TestResult {
    use kenwood_tmd750::types::{CatMemoryChannelRecord, MemoryChannelAddress, UrCallsign};

    const VFO: &str = "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    const STORED: &str = "995,0145190000,0000600000,2,2,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00,0";
    const EMPTY_UR: &str = "995,0145190000,0000600000,2,2,0,0,0,0,0,0,0,2,08,08,000,0,,0,00,0";
    let address = MemoryChannelAddress::regular(995)?;

    let mut mock = MockTransport::new();
    mock.expect(b"FO 0\r", format!("FO 0,{VFO}\r").as_bytes());
    scripted_live_identity(&mut mock);
    mock.expect(
        format!("ME {STORED}\r").as_bytes(),
        format!("ME {STORED}\r").as_bytes(),
    );
    mock.expect(b"ME 995\r", format!("ME {STORED}\r").as_bytes());
    mock.expect(
        format!("ME {EMPTY_UR}\r").as_bytes(),
        format!("ME {EMPTY_UR}\r").as_bytes(),
    );
    mock.expect(b"ME 995\r", format!("ME {STORED}\r").as_bytes());
    mock.expect(
        format!("ME {STORED}\r").as_bytes(),
        format!("ME {STORED}\r").as_bytes(),
    );
    mock.expect(
        b"ME 995\r",
        format!(
            "ME {}\r",
            STORED.replace("0145190000,0000", "0145200000,0000")
        )
        .as_bytes(),
    );
    let mut radio = Radio::new(mock);

    let channel = radio.get_channel_record(Band::A).await?;
    let request = CatMemoryChannelRecord::from_channel(channel.clone(), false);
    assert_eq!(
        radio.write_memory_channel(address, request.clone()).await?,
        request
    );

    let mut empty_ur = request.clone();
    empty_ur.channel.ur_call = UrCallsign::new("")?;
    let stored = radio.write_memory_channel(address, empty_ur).await?;
    assert_eq!(
        stored.channel.ur_call.as_str(),
        "CQCQCQ",
        "an empty URCALL reads back as CQCQCQ"
    );

    let differing = radio.write_memory_channel(address, request).await;
    assert!(
        matches!(
            differing,
            Err(Error::Protocol(ProtocolError::ReadbackMismatch {
                command: CommandName::Me,
                ref observed,
                ..
            })) if observed.starts_with("0145200000,")
        ),
        "a readback that differs in another field is refused: {differing:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_vfo_record_write_reports_echo_and_readback_differences_as_wire_fields() -> TestResult {
    use kenwood_tmd750::types::Frequency;

    const BEFORE: &str = "0223020000,0001600000,8,8,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00";
    let after = BEFORE.replacen("0223020000", "0223040000", 1);
    let mut mock = MockTransport::new();
    mock.expect(b"FO 1\r", format!("FO 1,{BEFORE}\r").as_bytes());
    scripted_live_identity(&mut mock);
    mock.expect(
        format!("FO 1,{after}\r").as_bytes(),
        format!("FO 1,{after}\r").as_bytes(),
    );
    mock.expect(b"FO 1\r", format!("FO 1,{after}\r").as_bytes());
    mock.expect(
        format!("FO 1,{BEFORE}\r").as_bytes(),
        format!("FO 1,{after}\r").as_bytes(),
    );
    mock.expect(
        format!("FO 1,{after}\r").as_bytes(),
        format!("FO 1,{after}\r").as_bytes(),
    );
    mock.expect(b"FO 1\r", format!("FO 1,{BEFORE}\r").as_bytes());
    let mut radio = Radio::new(mock);

    let original = radio.get_channel_record(Band::B).await?;
    let mut moved = original.clone();
    moved.receive_frequency = Frequency::new(223_040_000)?;
    radio.set_channel_record(Band::B, &moved).await?;

    let echoed = radio.set_channel_record(Band::B, &original).await;
    assert!(
        matches!(
            echoed,
            Err(Error::Protocol(ProtocolError::EchoMismatch {
                command: CommandName::Fo,
                ref requested,
                ref echoed,
            })) if requested == BEFORE && *echoed == after
        ),
        "{echoed:?}"
    );
    let read_back = radio.set_channel_record(Band::B, &moved).await;
    assert!(
        matches!(
            read_back,
            Err(Error::Protocol(ProtocolError::ReadbackMismatch {
                command: CommandName::Fo,
                ref requested,
                ref observed,
            })) if *requested == after && observed == BEFORE
        ),
        "{read_back:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn typed_reads_cover_the_remaining_state() -> TestResult {
    use kenwood_tmd750::types::{
        BeaconMethod, CurrentMemorySelector, DstarSlot, MemoryChannelAddress, PacketDataRate,
        TncMode, TncState, TuningMode, VoxMode,
    };

    let mut mock = MockTransport::new();
    mock.expect(b"AE\r", b"AE A1234567,K01\r");
    mock.expect(b"PS\r", b"PS 1\r");
    mock.expect(b"RT\r", b"RT 260921003607\r");
    mock.expect(
        b"FO 0\r",
        b"FO 0,0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
    );
    mock.expect(b"VM 0\r", b"VM 0,1\r");
    mock.expect(b"MR 0\r", b"MR AP \r");
    mock.expect(b"ME 000\r", b"N\r");
    mock.expect(b"DC 1\r", b"DC 1,,\r");
    mock.expect(b"DS\r", b"DS 1\r");
    mock.expect(b"AS\r", b"AS 0\r");
    mock.expect(b"PT\r", b"PT 2\r");
    mock.expect(b"TN\r", b"TN 0,0\r");
    mock.expect(b"VX\r", b"VX 0\r");
    mock.expect(b"SM 1\r", b"SM 1,9\r");
    mock.expect(b"BY 1\r", b"BY 1,1\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_serial_information().await?.serial_number(),
        "A1234567"
    );
    assert!(radio.get_power_status().await?);
    assert_eq!(
        radio.get_real_time_clock().await?.to_string(),
        "2026-09-21 00:36:07"
    );
    assert_eq!(
        radio
            .get_channel_record(Band::A)
            .await?
            .receive_frequency
            .as_hz(),
        145_190_000
    );
    assert_eq!(radio.get_tuning_mode(Band::A).await?, TuningMode::Memory);
    assert_eq!(
        radio.get_current_channel(Band::A).await?,
        CurrentMemorySelector::Aprs
    );
    assert_eq!(
        radio
            .get_memory_channel(MemoryChannelAddress::regular(0)?)
            .await?,
        None
    );
    assert_eq!(
        radio
            .get_dstar_callsign(DstarSlot::new(1)?)
            .await?
            .to_string(),
        "MY1 unset"
    );
    assert_eq!(radio.get_dstar_slot().await?, DstarSlot::new(1)?);
    assert_eq!(radio.get_packet_data_rate().await?, PacketDataRate::Bps1200);
    assert_eq!(radio.get_beacon_method().await?, BeaconMethod::Auto);
    assert_eq!(
        radio.get_tnc_mode().await?,
        TncState {
            mode: TncMode::Off,
            data_band: Band::A
        }
    );
    assert_eq!(radio.get_vox().await?, VoxMode::Off);
    assert_eq!(radio.get_smeter(Band::B).await?.as_raw(), 9);
    assert!(radio.get_busy(Band::B).await?);
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_different_radio_is_refused() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    let mut radio = Radio::new(mock);
    let refused = radio.identify().await;
    assert!(
        matches!(
            refused,
            Err(Error::Protocol(ProtocolError::UnexpectedIdentity { ref reply })) if reply == "TH-D75"
        ),
        "{refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn silence_is_a_timeout() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_hang(b"ID\r");
    let mut radio = Radio::new(mock);
    radio.set_timeout(std::time::Duration::from_millis(50));
    let result = radio.identify().await;
    assert!(matches!(result, Err(Error::Timeout { .. })), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn read_regions_handles_data_and_fill_replies() -> TestResult {
    let region = Region::new(8, 300)?;
    let pages = region.pages();
    let first = pages.first().copied().ok_or("no first page")?;
    let second = pages.get(1).copied().ok_or("no second page")?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(
        &Header::read_request(first),
        read_reply(first, &[0x11; 256]),
    );
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&Header::read_request(second), fill_reply(second, 0xEE));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut seen = Vec::new();
    let image = session
        .read_regions(&[region], |progress| seen.push(progress.done))
        .await?;
    assert_eq!(seen, vec![1, 2]);
    assert!(image.covers(region));
    let bytes = image.bytes(region).ok_or("region not readable")?;
    assert_eq!(bytes.len(), 292);
    assert_eq!(bytes.first().copied(), Some(0x11));
    assert_eq!(bytes.last().copied(), Some(0xEE));
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn echo_mismatch_is_a_protocol_error() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let other = Page::new(Address::new(56)?, 40)?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&Header::read_request(page), read_reply(other, &[0; 40]));
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session.read_regions(&[page.region()], |_| {}).await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::HeaderEcho { .. }))
        ),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn transmit_keys_and_receive_unkeys_in_that_order() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"TX\r", b"TX 1\r");
    mock.expect(b"RX\r", b"RX\r");
    let mut radio = Radio::new(mock);
    radio.transmit().await?;
    radio.receive().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn transmit_refuses_an_unqualified_target_before_keying() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    let mut radio = Radio::new(mock);
    let result = radio.transmit().await;
    assert!(
        matches!(result, Err(Error::UnsupportedWriteTarget { .. })),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn transmit_for_keys_waits_at_least_the_duration_then_unkeys() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"TX\r", b"TX 1\r");
    mock.expect(b"RX\r", b"RX\r");
    let mut radio = Radio::new(mock);
    let duration = std::time::Duration::from_millis(30);
    let started = tokio::time::Instant::now();
    radio.transmit_for(duration).await?;
    assert!(
        started.elapsed() >= duration,
        "the transmitter stays keyed for the requested duration"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn transmit_until_keys_before_awaiting_the_stop_and_then_releases() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"TX\r", b"TX 1\r");
    mock.expect(b"RX\r", b"RX\r");
    mock.expect(b"TX\r", b"TX 1\r");
    mock.expect(b"RX\r", b"RX\r");
    let mut radio = Radio::new(mock);
    let hold = std::time::Duration::from_millis(30);
    let started = tokio::time::Instant::now();
    radio.transmit_until(tokio::time::sleep(hold)).await?;
    assert!(
        started.elapsed() >= hold,
        "the transmitter stays keyed until the stop future resolves"
    );
    radio.transmit_until(std::future::ready(())).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn transmit_until_returns_a_refused_transmit_without_awaiting_the_stop() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"TX\r", b"N\r");
    let mut radio = Radio::new(mock);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        radio.transmit_until(std::future::pending::<()>()),
    )
    .await?;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::NotAvailable {
                command: CommandName::Tx
            }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_refused_transmit_is_not_available_and_sends_no_receive() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"TX\r", b"N\r");
    let mut radio = Radio::new(mock);
    let result = radio
        .transmit_for(std::time::Duration::from_millis(1))
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::NotAvailable {
                command: CommandName::Tx
            }))
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn tnc_mode_writes_echo_and_read_back() -> TestResult {
    use kenwood_tmd750::types::{SelectableTncMode, TncMode};

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"TN 2,1\r", b"TN 2,1\r");
    mock.expect(b"TN\r", b"TN 2,1\r");
    mock.expect(b"TN 0,0\r", b"TN 0,0\r");
    mock.expect(b"TN\r", b"TN 0,0\r");
    let mut radio = Radio::new(mock);
    radio.set_tnc_mode(SelectableTncMode::Kiss, Band::B).await?;
    radio.set_tnc_mode(SelectableTncMode::Off, Band::A).await?;
    radio.into_transport().assert_complete();
    assert_eq!(
        TncMode::from(SelectableTncMode::Kiss),
        TncMode::Kiss,
        "a selectable mode names its reported mode"
    );
    Ok(())
}

#[tokio::test]
async fn vox_writes_echo_read_back_and_settle_before_returning() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"VX 1\r", b"VX 1\r");
    mock.expect(b"VX\r", b"VX 1\r");
    mock.expect(b"VX 0\r", b"VX 0\r");
    mock.expect(b"VX\r", b"VX 0\r");
    let mut radio = Radio::new(mock);
    let started = tokio::time::Instant::now();
    radio.set_vox(true).await?;
    assert!(
        started.elapsed() >= kenwood_tmd750::radio::VOX_SETTLE,
        "a VX write returns only after the settle time"
    );
    radio.set_vox(false).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn an_aprs_beacon_request_sends_be_and_requires_its_acknowledgement() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"BE\r", b"BE\r");
    mock.expect(b"BE\r", b"N\r");
    let mut radio = Radio::new(mock);
    radio.send_aprs_beacon().await?;
    let refused = radio.send_aprs_beacon().await;
    assert!(
        matches!(
            refused,
            Err(Error::Protocol(ProtocolError::NotAvailable {
                command: CommandName::Be
            }))
        ),
        "{refused:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn power_off_sends_ps_0_and_requires_its_echo() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"PS 0\r", b"PS 0\r");
    let mut radio = Radio::new(mock);
    radio.power_off().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn receive_still_releases_the_transmitter_after_an_unanswered_transmit() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect_hang(b"TX\r");
    mock.expect(b"RX\r", b"RX\r");
    mock.expect(b"MD 0\r", b"MD 0,0\r");
    let mut radio = Radio::new(mock);
    radio.set_timeout(std::time::Duration::from_millis(20));
    let keyed = radio.transmit().await;
    assert!(
        matches!(
            keyed,
            Err(Error::Timeout {
                operation: TimeoutOperation::Command(CommandName::Tx),
                ..
            })
        ),
        "{keyed:?}"
    );
    let blocked = radio.get_operating_mode(Band::A).await;
    assert!(
        matches!(
            blocked,
            Err(Error::Mcp(kenwood_tmd750::McpError::RecoveryRequired))
        ),
        "an uncertain CAT connection refuses ordinary commands: {blocked:?}"
    );
    radio.receive().await?;
    assert_eq!(
        radio.get_operating_mode(Band::A).await?,
        OperatingMode::Fm,
        "a correlated RX reply resynchronizes the connection"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn transmit_until_releases_a_key_up_it_could_not_confirm() -> TestResult {
    for malformed in [false, true] {
        let mut mock = MockTransport::new();
        scripted_live_identity(&mut mock);
        if malformed {
            mock.expect(b"TX\r", b"TX 0\r");
        } else {
            mock.expect_hang(b"TX\r");
        }
        mock.expect(b"RX\r", b"RX\r");
        let mut radio = Radio::new(mock);
        radio.set_timeout(std::time::Duration::from_millis(20));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            radio.transmit_until(std::future::pending::<()>()),
        )
        .await?;
        assert!(
            matches!(
                result,
                Err(Error::Timeout {
                    operation: TimeoutOperation::Command(CommandName::Tx),
                    ..
                } | Error::Protocol(ProtocolError::FieldParse { .. }))
            ),
            "the TX failure returns once RX released the transmitter: {result:?}"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn an_unacknowledged_release_is_the_error_that_may_leave_the_transmitter_keyed() -> TestResult
{
    for acknowledged in [true, false] {
        let mut mock = MockTransport::new();
        scripted_live_identity(&mut mock);
        if acknowledged {
            mock.expect(b"TX\r", b"TX 1\r");
        } else {
            mock.expect_hang(b"TX\r");
        }
        mock.expect_hang(b"RX\r");
        let mut radio = Radio::new(mock);
        radio.set_timeout(std::time::Duration::from_millis(20));
        let result = radio.transmit_until(std::future::ready(())).await;
        let Err(Error::ReleaseUnconfirmed { transmit, release }) = &result else {
            return Err(format!("expected ReleaseUnconfirmed, got {result:?}").into());
        };
        assert_eq!(
            transmit.is_none(),
            acknowledged,
            "the TX failure is kept when there was one: {result:?}"
        );
        assert!(
            matches!(
                release.as_ref(),
                Error::Timeout {
                    operation: TimeoutOperation::Command(CommandName::Rx),
                    ..
                }
            ),
            "{result:?}"
        );
        radio.into_transport().assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn receive_is_refused_while_programming_mode_is_uncertain() -> TestResult {
    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect_partial_then_hang(b"0M PROGRAM\r", b"0");
    let mut radio = Radio::new(mock);
    radio.set_timeout(std::time::Duration::from_millis(20));
    assert!(
        radio.enter_mcp().await.is_err(),
        "the entry reply never completes"
    );
    let released = radio.receive().await;
    assert!(
        matches!(
            released,
            Err(Error::Mcp(kenwood_tmd750::McpError::RecoveryRequired))
        ),
        "no CAT bytes are sent into uncertain MCP framing: {released:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}
