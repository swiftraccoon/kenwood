//! Parsers for the `FO` and `ME` channel records.

use crate::error::ProtocolError;
use crate::types::{
    CHANNEL_FIELD_COUNT, CatChannelRecord, CatMemoryChannelRecord, CrossToneField, DcsCode,
    DigitalCode, DigitalSquelch, Frequency, MemoryChannelAddress, OperatingMode, ShiftDirection,
    StepSize, ToneCode, ToneMode, TransmitSetting, UrCallsign,
};

use super::fields::{boolean, decimal_u8, field_error, fixed_decimal_u8, split_exact};

/// Number of fields in an `ME` reply: the address, the shared record, the
/// split flag and the scan lockout flag.
pub const MEMORY_FIELD_COUNT: usize = CHANNEL_FIELD_COUNT + 3;

/// Parse the 18 shared channel fields in wire order.
pub(crate) fn parse_channel_fields(
    fields: &[&str; CHANNEL_FIELD_COUNT],
    command: &'static str,
) -> Result<CatChannelRecord, ProtocolError> {
    let [
        receive_frequency,
        transmit,
        receive_step,
        transmit_step,
        mode,
        tone,
        ctcss,
        dcs,
        cross,
        reverse,
        shift,
        tone_code,
        ctcss_code,
        dcs_code,
        cross_tone,
        ur_call,
        digital_squelch,
        digital_code,
    ] = *fields;
    let validation = |field: &'static str| {
        move |error: crate::error::ValidationError| field_error(command, field, error.to_string())
    };
    let signaling = ToneMode::from_flags([
        boolean(tone, command, "tone")?,
        boolean(ctcss, command, "ctcss")?,
        boolean(dcs, command, "dcs")?,
        boolean(cross, command, "cross_tone")?,
    ])
    .map_err(validation("tone_mode"))?;
    Ok(CatChannelRecord {
        receive_frequency: Frequency::from_wire_str(receive_frequency)
            .map_err(validation("receive_frequency"))?,
        transmit: TransmitSetting::from_wire_str(transmit).map_err(validation("transmit"))?,
        receive_step: StepSize::from_wire_str(receive_step).map_err(validation("receive_step"))?,
        transmit_step: StepSize::from_wire_str(transmit_step)
            .map_err(validation("transmit_step"))?,
        mode: OperatingMode::from(decimal_u8(mode, command, "mode")?),
        tone_mode: signaling,
        reverse: boolean(reverse, command, "reverse")?,
        shift: ShiftDirection::try_from(decimal_u8(shift, command, "shift")?)
            .map_err(validation("shift"))?,
        tone_code: ToneCode::new(fixed_decimal_u8::<2>(tone_code, command, "tone_code")?)
            .map_err(validation("tone_code"))?,
        ctcss_code: ToneCode::new(fixed_decimal_u8::<2>(ctcss_code, command, "ctcss_code")?)
            .map_err(validation("ctcss_code"))?,
        dcs_code: DcsCode::new(fixed_decimal_u8::<3>(dcs_code, command, "dcs_code")?)
            .map_err(validation("dcs_code"))?,
        cross_tone: CrossToneField::from_wire_str(cross_tone).map_err(validation("cross_tone"))?,
        ur_call: UrCallsign::new(ur_call).map_err(validation("ur_call"))?,
        digital_squelch: DigitalSquelch::try_from(decimal_u8(
            digital_squelch,
            command,
            "digital_squelch",
        )?)
        .map_err(validation("digital_squelch"))?,
        digital_code: DigitalCode::new(fixed_decimal_u8::<2>(
            digital_code,
            command,
            "digital_code",
        )?)
        .map_err(validation("digital_code"))?,
    })
}

/// Parse an `ME` payload: address, ten fields through reverse, split, shift,
/// seven code fields, and scan lockout.
///
/// # Errors
///
/// Returns [`ProtocolError::FieldCount`] for any count other than 21 fields
/// and [`ProtocolError::FieldParse`] naming the malformed field.
pub fn parse_memory_channel(
    payload: &str,
) -> Result<(MemoryChannelAddress, CatMemoryChannelRecord), ProtocolError> {
    let fields = split_exact::<MEMORY_FIELD_COUNT>(payload, "ME")?;
    let address = MemoryChannelAddress::from_wire_str(fields[0])
        .map_err(|error| field_error("ME", "address", error.to_string()))?;
    let shared: [&str; CHANNEL_FIELD_COUNT] = [
        fields[1], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7], fields[8],
        fields[9], fields[10], fields[12], fields[13], fields[14], fields[15], fields[16],
        fields[17], fields[18], fields[19],
    ];
    let channel = parse_channel_fields(&shared, "ME")?;
    let split = boolean(fields[11], "ME", "split")?;
    let scan_lockout = boolean(fields[20], "ME", "scan_lockout")?;
    Ok((
        address,
        CatMemoryChannelRecord {
            channel,
            split,
            scan_lockout,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OffsetFrequency;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const VFO_A: &str = "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00";
    const DR_REPEATER: &str = "0448375000,0005000000,2,2,4,0,0,0,0,0,2,24,08,000,8,CQCQCQ,0,00";
    const AIR_BAND: &str = "0121500000,0000000000,6,6,2,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00";
    const APRS_CHANNEL: &str = "0144390000,0000000000,6,0,0,0,0,0,0,0,0,00,00,000,0,,0,00";

    fn fields(payload: &str) -> Result<[&str; CHANNEL_FIELD_COUNT], ProtocolError> {
        split_exact::<CHANNEL_FIELD_COUNT>(payload, "FO")
    }

    #[test]
    fn observed_records_parse_and_round_trip() -> TestResult {
        for payload in [VFO_A, DR_REPEATER, AIR_BAND, APRS_CHANNEL] {
            let record = parse_channel_fields(&fields(payload)?, "FO")?;
            assert_eq!(record.to_wire_fields(), payload, "{payload}");
        }
        let vfo = parse_channel_fields(&fields(VFO_A)?, "FO")?;
        assert_eq!(vfo.receive_frequency.as_hz(), 145_190_000);
        assert_eq!(
            vfo.transmit,
            TransmitSetting::Offset(OffsetFrequency::new(600_000)?)
        );
        assert_eq!(vfo.receive_step, StepSize::Hz5000);
        assert_eq!(vfo.mode, OperatingMode::Fm);
        assert_eq!(vfo.tone_mode, ToneMode::Off);
        assert_eq!(vfo.shift, ShiftDirection::Minus);
        assert_eq!(vfo.tone_code.as_raw(), 8);
        assert_eq!(vfo.ur_call.as_str(), "CQCQCQ");
        let repeater = parse_channel_fields(&fields(DR_REPEATER)?, "FO")?;
        assert_eq!(repeater.mode, OperatingMode::Dr);
        assert_eq!(repeater.cross_tone.as_raw(), 8);
        assert_eq!(repeater.tone_code.as_raw(), 24);
        let air = parse_channel_fields(&fields(AIR_BAND)?, "FO")?;
        assert_eq!(air.mode, OperatingMode::Am);
        assert_eq!(air.receive_step, StepSize::Hz12500);
        let aprs = parse_channel_fields(&fields(APRS_CHANNEL)?, "FO")?;
        assert_eq!(aprs.transmit_step, StepSize::Hz1000);
        assert_eq!(aprs.ur_call.as_str(), "");
        Ok(())
    }

    #[test]
    fn malformed_fields_name_the_command_and_field() -> TestResult {
        let cases = [
            (
                "145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00",
                "receive_frequency",
            ),
            (
                "0145190000,0000600000,D,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00",
                "receive_step",
            ),
            (
                "0145190000,0000600000,2,2,0,1,1,0,0,0,2,08,08,000,0,CQCQCQ,0,00",
                "tone_mode",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,3,08,08,000,0,CQCQCQ,0,00",
                "shift",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,2,8,08,000,0,CQCQCQ,0,00",
                "tone_code",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,104,0,CQCQCQ,0,00",
                "dcs_code",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,G,CQCQCQ,0,00",
                "cross_tone",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,cqcqcq,0,00",
                "ur_call",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,3,00",
                "digital_squelch",
            ),
            (
                "0145190000,0000600000,2,2,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,0",
                "digital_code",
            ),
        ];
        for (payload, expected_field) in cases {
            let result = parse_channel_fields(&fields(payload)?, "FO");
            assert!(
                matches!(
                    result,
                    Err(ProtocolError::FieldParse { command: "FO", field, .. }) if field == expected_field
                ),
                "{payload}: {result:?}"
            );
        }
        let short = fields("0145190000,0000600000");
        assert!(
            matches!(
                short,
                Err(ProtocolError::FieldCount {
                    command: "FO",
                    expected: 18,
                    actual: 2
                })
            ),
            "{short:?}"
        );
        Ok(())
    }

    #[test]
    fn memory_records_carry_split_and_lockout_around_the_shared_fields() -> TestResult {
        let payload = "021,0446000000,0447000000,2,2,0,0,0,0,0,0,1,0,08,08,000,0,CQCQCQ,0,00,1";
        let (address, record) = parse_memory_channel(payload)?;
        assert_eq!(address, MemoryChannelAddress::Regular(21));
        assert!(record.split);
        assert!(record.scan_lockout);
        assert_eq!(record.channel.shift, ShiftDirection::Simplex);
        assert_eq!(
            record.channel.transmit,
            TransmitSetting::Split(Frequency::new(447_000_000)?)
        );
        let bad_address = parse_memory_channel(
            "21,0446000000,0447000000,2,2,0,0,0,0,0,0,1,0,08,08,000,0,CQCQCQ,0,00,1",
        );
        assert!(
            matches!(
                bad_address,
                Err(ProtocolError::FieldParse {
                    command: "ME",
                    field: "address",
                    ..
                })
            ),
            "{bad_address:?}"
        );
        let short = parse_memory_channel("021,0446000000");
        assert!(
            matches!(
                short,
                Err(ProtocolError::FieldCount {
                    command: "ME",
                    expected: 21,
                    actual: 2
                })
            ),
            "{short:?}"
        );
        Ok(())
    }
}
