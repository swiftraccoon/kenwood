//! Identity proof and MCP exchanges over a scripted mock transport.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{
    ACK, BytePatch, ENTER, EXIT, PagePatch, read_request, write_request,
};
use kenwood_tmd750::radio::Radio;
use kenwood_tmd750::{
    Address, Band, DvGatewayMode, Error, McpError, OperatingMode, Page, ProtocolError, Region,
    SelectableMode,
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
    let mut reply = write_request(page).to_vec();
    reply.extend_from_slice(data);
    reply
}

fn fill_reply(page: Page, fill: u8) -> Vec<u8> {
    let mut reply = write_request(page).to_vec();
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
        OperatingMode::Unqualified(7)
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
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
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
        matches!(result, Err(Error::UnsupportedCatWriteTarget { .. })),
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
            Err(Error::UnsupportedCatWriteTarget {
                ref actual_firmware,
                ref actual_radio_type,
                ..
            }) if actual_firmware == "1.02" && actual_radio_type == "K,2,2"
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
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "write echo equal to the request",
                ..
            }))
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
                command: "MD"
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
            Err(Error::Protocol(ProtocolError::Rejected { command: "MD" }))
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
    use kenwood_tmd750::types::{DstarCallsignEntry, DstarSlot};

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    mock.expect(b"DC 6,KQ4NIT,RIG\r", b"DC 6,KQ4NIT,RIG\r");
    mock.expect(b"DC 6\r", b"DC 6,KQ4NIT,RIG\r");
    mock.expect(b"DC 6,,\r", b"DC 6,,\r");
    mock.expect(b"DC 6\r", b"DC 6,,\r");
    let mut radio = Radio::new(mock);
    let slot = DstarSlot::new(6)?;
    radio
        .set_dstar_callsign(&DstarCallsignEntry::new(slot, "KQ4NIT", "RIG")?)
        .await?;
    radio.clear_dstar_callsign(slot).await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn dstar_callsign_write_refuses_a_mismatched_echo() -> TestResult {
    use kenwood_tmd750::types::{DstarCallsignEntry, DstarSlot};

    let mut mock = MockTransport::new();
    scripted_live_identity(&mut mock);
    // A truncated memo echo is not the requested entry, so the write is a
    // protocol error and no readback follows.
    mock.expect(b"DC 6,KQ4NIT,RIG\r", b"DC 6,KQ4NIT,RI\r");
    let mut radio = Radio::new(mock);
    let result = radio
        .set_dstar_callsign(&DstarCallsignEntry::new(
            DstarSlot::new(6)?,
            "KQ4NIT",
            "RIG",
        )?)
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
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
            Err(Error::Protocol(ProtocolError::UnexpectedResponse { .. }))
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
                step: "DW",
                ..
            })
        ),
        "{result:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn typed_reads_cover_the_remaining_state() -> TestResult {
    use kenwood_tmd750::types::{
        BeaconMethod, CurrentMemorySelector, DstarSlot, MemoryChannelAddress, PacketDataRate,
        TncMode, TuningMode, VoxMode,
    };

    let mut mock = MockTransport::new();
    mock.expect(b"AE\r", b"AE C6210439,K01\r");
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
        "C6210439"
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
    assert_eq!(radio.get_tnc_mode().await?, (TncMode::Off, Band::A));
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
    mock.expect(&read_request(first), &read_reply(first, &[0x11; 256]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&read_request(second), &fill_reply(second, 0xEE));
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
    mock.expect(&read_request(page), &read_reply(other, &[0; 40]));
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

fn patch_for(page: Page) -> Result<PagePatch, Box<dyn std::error::Error>> {
    Ok(PagePatch::new(page, vec![BytePatch::new(2, 0xFF, 0x42)?])?)
}

#[tokio::test]
async fn verified_write_reads_patches_writes_and_reads_back() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page)?;
    let mut written = vec![0x00; 40];
    patch.apply(&mut written)?;
    let mut expected_write = write_request(page).to_vec();
    expected_write.extend_from_slice(&written);
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(page, &[0x00; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&expected_write, &[ACK]);
    mock.expect(&read_request(page), &read_reply(page, &written));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let report = session.write_pages_verified(&[patch], |_| {}).await?;
    assert_eq!(report.verified_pages, vec![page]);
    assert!(report.possibly_written_pages.is_empty());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_read_back_mismatch_keeps_the_page_in_the_journal() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page)?;
    let mut written = vec![0x00; 40];
    patch.apply(&mut written)?;
    let mut expected_write = write_request(page).to_vec();
    expected_write.extend_from_slice(&written);
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(page, &[0x00; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&expected_write, &[ACK]);
    mock.expect(&read_request(page), &read_reply(page, &[0x00; 40]));
    mock.expect(&[ACK], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session.write_pages_verified(&[patch], |_| {}).await;
    match result {
        Err(Error::Mcp(McpError::Interrupted {
            possibly_written,
            verified,
            source,
            ..
        })) => {
            assert_eq!((possibly_written, verified), (1, 0));
            assert!(
                matches!(
                    *source,
                    Error::Mcp(McpError::VerifyMismatch { offset: 2, .. })
                ),
                "{source:?}"
            );
        }
        other => return Err(format!("expected an interrupted write, got {other:?}").into()),
    }
    assert_eq!(session.journal().possibly_written, vec![page]);
    Ok(())
}

#[tokio::test]
async fn pages_outside_the_writable_regions_are_refused_before_any_write() -> TestResult {
    let outside = Page::new(Address::new(400_000)?, 256)?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session
        .write_pages_verified(&[patch_for(outside)?], |_| {})
        .await;
    assert!(
        matches!(
            result,
            Err(Error::Mcp(McpError::PageNotWritable {
                address: 400_000,
                ..
            }))
        ),
        "{result:?}"
    );
    assert!(session.journal().possibly_written.is_empty());
    Ok(())
}

#[tokio::test]
async fn recovery_reports_which_journaled_pages_carry_the_patch() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch_for(page)?;
    let mut applied = vec![0x00; 40];
    patch.apply(&mut applied)?;
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock);
    mock.expect(ENTER, b"0M\r");
    mock.expect(&read_request(page), &read_reply(page, &applied));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let journal = kenwood_tmd750::radio::programming::McpJournal {
        possibly_written: vec![page],
        verified: Vec::new(),
    };
    let report = radio.recover(&journal, &[patch]).await?;
    assert_eq!(report.applied, vec![page]);
    assert!(report.pending.is_empty());
    radio.into_transport().assert_complete();
    Ok(())
}
