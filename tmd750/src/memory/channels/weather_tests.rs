use super::*;
use crate::types::OperatingMode;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One weather channel as firmware 1.02 stores it.
#[derive(Clone, Copy)]
struct Stored {
    number: u8,
    flag: [u8; 4],
    /// Record bytes 0..15; bytes 15..39 are `0xFF` and byte 39 is zero.
    head: [u8; 15],
    name: &'static str,
    hz: u32,
}

const WX_1: Stored = Stored {
    number: 1,
    flag: [0x01, 0x00, 0x20, 0x00],
    head: [
        0xF0, 0x50, 0xB0, 0x09, 0x00, 0x00, 0x00, 0x00, 0x22, 0x01, 0x10, 0x08, 0x08, 0x00, 0x70,
    ],
    name: "WX  1",
    hz: 162_550_000,
};

const WX_10: Stored = Stored {
    number: 10,
    flag: [0x01, 0x00, 0x20, 0x00],
    head: [
        0xF8, 0x60, 0xBB, 0x09, 0x00, 0x00, 0x00, 0x00, 0x22, 0x01, 0x10, 0x08, 0x08, 0x00, 0x70,
    ],
    name: "WX 10",
    hz: 163_275_000,
};

fn record(head: [u8; 15]) -> [u8; CHANNEL_RECORD_SIZE] {
    let mut bytes = [0xFF; CHANNEL_RECORD_SIZE];
    for (destination, source) in bytes.iter_mut().zip(head) {
        *destination = source;
    }
    if let Some(code) = bytes.last_mut() {
        *code = 0;
    }
    bytes
}

fn name(text: &str) -> [u8; CHANNEL_NAME_SIZE] {
    let mut bytes = [0; CHANNEL_NAME_SIZE];
    for (destination, source) in bytes.iter_mut().zip(text.bytes()) {
        *destination = source;
    }
    bytes
}

fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
    image
        .get_mut(offset..offset + bytes.len())
        .ok_or("offset inside the image")?
        .copy_from_slice(bytes);
    Ok(())
}

#[test]
fn channel_access_reads_all_ten_from_the_tables() -> TestResult {
    let mut image = vec![0xFF; crate::types::IMAGE_LENGTH];
    for index in 0..10 {
        let stored = if index == 9 { WX_10 } else { WX_1 };
        put(&mut image, 0x3138 + 4 * index, &stored.flag)?;
        put(&mut image, 0x1_44E0 + 16 * index, &name(stored.name))?;
        let page = (1102 + index) / 6;
        let slot = (1102 + index) % 6;
        put(
            &mut image,
            0x4000 + 256 * page + 40 * slot,
            &record(stored.head),
        )?;
    }
    let image = crate::memory::MemoryImage::from_bytes(image)?;
    let access = crate::memory::ChannelAccess::from_memory_image(&image);
    let entries: Vec<WeatherChannelEntry> = access.weather_channels().collect::<Result<_, _>>()?;
    assert_eq!(entries.len(), 10);
    let last = access.weather_channel(WeatherChannel::new(10)?)?;
    assert_eq!(last.to_string(), "WX 10: 163.275000 MHz");
    assert_eq!(last.name().text(), "WX 10");
    assert_eq!(
        entries.first().ok_or("WX 1")?.to_string(),
        "WX 1: 162.550000 MHz"
    );
    Ok(())
}

#[test]
fn weather_channels_follow_the_aprs_channel() -> TestResult {
    assert!(WeatherChannel::new(0).is_err());
    assert!(WeatherChannel::new(11).is_err());
    let first = WeatherChannel::new(1)?;
    let last = WeatherChannel::new(10)?;
    assert_eq!(first.index(), 1102);
    assert_eq!(last.index(), 1111);
    assert_eq!(first.flag_address(), 0x3138);
    assert_eq!(first.data_address(), 0xF7A0);
    assert_eq!(first.name_address(), 0x1_44E0);
    assert_eq!(last.data_address(), 0xF928);
    assert_eq!(WeatherChannel::all().count(), 10);
    assert_eq!(last.to_string(), "WX 10");
    Ok(())
}

#[test]
fn stored_weather_records_decode_losslessly() -> TestResult {
    for stored in [WX_1, WX_10] {
        let channel = WeatherChannel::new(stored.number)?;
        let entry = WeatherChannelEntry::from_records(
            channel,
            &stored.flag,
            &record(stored.head),
            &name(stored.name),
        )?;
        let decoded = entry.record().ok_or("a stored weather record")?;
        assert_eq!(decoded.receive_frequency.as_hz(), stored.hz);
        assert_eq!(decoded.mode, OperatingMode::Fm);
        assert_eq!(decoded.to_bytes(), record(stored.head));
        assert_eq!(entry.flag().group(), 32);
        assert!(!entry.flag().scan_lockout());
        assert_eq!(entry.name().text(), stored.name);
    }
    let empty = WeatherChannelEntry::from_records(
        WeatherChannel::new(2)?,
        &[0xFF, 0x00, 0x20, 0x00],
        &[0xFF; 40],
        &[0; 16],
    )?;
    assert!(empty.record().is_none());
    assert_eq!(empty.to_string(), "WX 2: empty");
    Ok(())
}
