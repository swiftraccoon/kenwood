//! Batch memory-channel programming over scripted transports: CAT stores and
//! clears, then channel names in one MCP session.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::memory::{ChannelNamePlan, ChannelNameText};
use kenwood_tmd750::protocol::mcp::{ACK, Header};
use kenwood_tmd750::radio::{ChannelBatchError, ChannelWrite, Radio};
use kenwood_tmd750::types::{CatMemoryChannelRecord, MemoryChannelAddress, PhysicalChannel};
use kenwood_tmd750::{Address, Band, CommandName, Error, Page, Progress, ProtocolError};
use kenwood_transport::MockTransport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VFO: &str = "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
const STORED_995: &str = "995,0145190000,0000600000,2,2,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00,0";
const STORED_996: &str = "996,0145190000,0000600000,2,2,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00,0";

fn scripted_identity(mock: &mut MockTransport, firmware: &str) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

/// A channel record taken from a scripted `FO 0` read.
async fn record(radio: &mut Radio<MockTransport>) -> Result<CatMemoryChannelRecord, Error> {
    Ok(CatMemoryChannelRecord::from_channel(
        radio.get_channel_record(Band::A).await?,
        false,
    ))
}

fn page(address: u32) -> Result<Page, Box<dyn std::error::Error>> {
    Ok(Page::new(Address::new(address)?, 256)?)
}

fn read(mock: &mut MockTransport, page: Page, data: &[u8]) {
    let mut reply = Header::write_request(page).to_vec();
    reply.extend_from_slice(data);
    mock.expect(&Header::read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

#[tokio::test]
async fn a_batch_stores_and_clears_each_channel_in_order() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FO 0\r", format!("FO 0,{VFO}\r").as_bytes());
    scripted_identity(&mut mock, "1.02");
    mock.expect(
        format!("ME {STORED_995}\r").as_bytes(),
        format!("ME {STORED_995}\r").as_bytes(),
    );
    mock.expect(b"ME 995\r", format!("ME {STORED_995}\r").as_bytes());
    mock.expect(b"ME 996,\r", b"ME 996\r");
    mock.expect(b"ME 996\r", b"N\r");
    let mut radio = Radio::new(mock);
    let stored = record(&mut radio).await?;
    let first = MemoryChannelAddress::regular(995)?;
    let second = MemoryChannelAddress::regular(996)?;

    let mut seen = Vec::new();
    let programmed = radio
        .program_memory_channels(
            &[
                (first, ChannelWrite::Store(stored.clone())),
                (second, ChannelWrite::Clear),
            ],
            |progress| seen.push(progress),
        )
        .await?;
    assert_eq!(programmed, [(first, Some(stored)), (second, None)]);
    assert_eq!(
        seen,
        [
            Progress { done: 1, total: 2 },
            Progress { done: 2, total: 2 }
        ]
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn a_batch_stops_at_its_first_failure_and_reports_what_finished() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FO 0\r", format!("FO 0,{VFO}\r").as_bytes());
    scripted_identity(&mut mock, "1.02");
    mock.expect(
        format!("ME {STORED_995}\r").as_bytes(),
        format!("ME {STORED_995}\r").as_bytes(),
    );
    mock.expect(b"ME 995\r", format!("ME {STORED_995}\r").as_bytes());
    mock.expect(format!("ME {STORED_996}\r").as_bytes(), b"N\r");
    let mut radio = Radio::new(mock);
    let stored = record(&mut radio).await?;
    let first = MemoryChannelAddress::regular(995)?;
    let second = MemoryChannelAddress::regular(996)?;
    let third = MemoryChannelAddress::regular(997)?;

    let result = radio
        .program_memory_channels(
            &[
                (first, ChannelWrite::Store(stored.clone())),
                (second, ChannelWrite::Store(stored.clone())),
                (third, ChannelWrite::Clear),
            ],
            |_| {},
        )
        .await;
    let Err(ChannelBatchError {
        completed,
        failed,
        source,
    }) = result
    else {
        return Err(format!("the rejected write must stop the batch: {result:?}").into());
    };
    assert_eq!(completed, [(first, Some(stored))]);
    assert_eq!(failed, second);
    assert!(
        matches!(
            source.as_ref(),
            Error::Protocol(ProtocolError::NotAvailable {
                command: CommandName::Me
            })
        ),
        "{source:?}"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn channel_names_are_read_fresh_then_compare_exchanged() -> TestResult {
    let names_page = page(0x1_0000)?;
    let before = [0_u8; 256];
    let mut after = before;
    after
        .get_mut(..10)
        .ok_or("the first name")?
        .copy_from_slice(b"Repeater 1");
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock, "1.02");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, names_page, &before);
    read(&mut mock, names_page, &before);
    let mut frame = Header::write_request(names_page).to_vec();
    frame.extend_from_slice(&after);
    mock.expect(&frame, &[ACK]);
    read(&mut mock, names_page, &after);
    mock.expect(b"E", &[ACK]);
    let plan = ChannelNamePlan::new([(
        PhysicalChannel::new(0)?,
        Some(ChannelNameText::new("Repeater 1")?),
    )])?;

    let mut radio = Radio::new(mock);
    let _identity = radio.identify().await?;
    let mut session = radio.enter_mcp().await?;
    let mut intents = 0;
    let report = session
        .write_channel_names(
            &plan,
            |_| {
                intents += 1;
                Ok(())
            },
            |_| {},
        )
        .await?;
    assert_eq!(report.verified_pages, [names_page]);
    assert_eq!(intents, 1);
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn names_already_stored_are_read_but_never_written() -> TestResult {
    let names_page = page(0x1_0000)?;
    let mut stored = [0_u8; 256];
    stored
        .get_mut(..10)
        .ok_or("the first name")?
        .copy_from_slice(b"Repeater 1");
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock, "1.02");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, names_page, &stored);
    mock.expect(b"E", &[ACK]);
    let plan = ChannelNamePlan::new([(
        PhysicalChannel::new(0)?,
        Some(ChannelNameText::new("Repeater 1")?),
    )])?;

    let mut radio = Radio::new(mock);
    let _identity = radio.identify().await?;
    let mut session = radio.enter_mcp().await?;
    let report = session
        .write_channel_names(&plan, |_| Ok(()), |_| {})
        .await?;
    assert!(report.verified_pages.is_empty() && report.compared_pages.is_empty());
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn channel_names_refuse_a_radio_outside_the_write_targets_before_page_traffic() -> TestResult
{
    let mut mock = MockTransport::new();
    scripted_identity(&mut mock, "1.03");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock.expect(b"E", &[ACK]);
    let plan = ChannelNamePlan::new([(PhysicalChannel::new(0)?, None)])?;

    let mut radio = Radio::new(mock);
    let _identity = radio.identify().await?;
    let mut session = radio.enter_mcp().await?;
    let result = session.write_channel_names(&plan, |_| Ok(()), |_| {}).await;
    assert!(
        matches!(result, Err(Error::UnsupportedWriteTarget { .. })),
        "{result:?}"
    );
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}
