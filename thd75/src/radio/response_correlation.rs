//! Exact correlation between one CAT command and a parsed response.
//!
//! CAT has no request identifiers. Auto-info notifications and late replies
//! can therefore share a mnemonic with the command currently in flight. A
//! mnemonic match is only the first check: identifying fields on reads and
//! every echoed field on writes must also agree before a response can complete
//! the command.

use crate::protocol::{Command, Response};

/// How completely a response proves that it answers a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponseCorrelation {
    /// The response has the expected shape and every correlating field agrees.
    Exact,
    /// The firmware acknowledged the write without echoing the resulting state.
    Acknowledged,
    /// The response has the command's mnemonic but not its requested identity.
    Mismatch,
}

impl ResponseCorrelation {
    /// Whether this response is allowed to complete the command.
    pub(super) const fn completes_command(self) -> bool {
        matches!(self, Self::Exact | Self::Acknowledged)
    }
}

const fn exact_if(matches: bool) -> ResponseCorrelation {
    if matches {
        ResponseCorrelation::Exact
    } else {
        ResponseCorrelation::Mismatch
    }
}

/// Correlate one parsed response with the command that is currently in flight.
///
/// The outer match is deliberately exhaustive over [`Command`]. Adding a new
/// CAT operation therefore requires an explicit correlation rule instead of
/// silently falling back to mnemonic-only matching.
#[expect(
    clippy::too_many_lines,
    reason = "This is the exhaustive CAT command/response contract. Keeping one visibly complete \
              match makes omissions auditable and causes every new Command variant to require an \
              explicit correlation rule."
)]
pub(super) fn correlate(command: &Command, response: &Response) -> ResponseCorrelation {
    match command {
        // Core
        Command::GetFrequency { band } => exact_if(matches!(
            response,
            Response::Frequency {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::GetFrequencyFull { band } => exact_if(matches!(
            response,
            Response::FrequencyFull {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::GetFirmwareVersion => {
            exact_if(matches!(response, Response::FirmwareVersion { .. }))
        }
        Command::GetPowerStatus => exact_if(matches!(response, Response::PowerStatus { .. })),
        Command::GetRadioId => exact_if(matches!(response, Response::RadioId { .. })),
        Command::GetPowerLevel { band } => exact_if(matches!(
            response,
            Response::PowerLevel {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::SetPowerLevel { band, level } => exact_if(matches!(
            response,
            Response::PowerLevel {
                band: response_band,
                level: response_level,
            } if response_band == band && response_level == level
        )),
        Command::GetBand => exact_if(matches!(response, Response::Band { .. })),
        Command::SetBand { band } => exact_if(matches!(
            response,
            Response::Band {
                band: response_band,
            } if response_band == band
        )),
        Command::GetTuningMode { band } => exact_if(matches!(
            response,
            Response::TuningMode {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::SetTuningMode { band, mode } => exact_if(matches!(
            response,
            Response::TuningMode {
                band: response_band,
                mode: response_mode,
            } if response_band == band && response_mode == mode
        )),
        Command::GetFmRadio => exact_if(matches!(response, Response::FmRadio { .. })),

        // VFO
        Command::GetAfGain => exact_if(matches!(response, Response::AfGain { .. })),
        Command::SetAfGain { level } => exact_if(matches!(
            response,
            Response::AfGain {
                level: response_level,
            } if response_level == level
        )),
        Command::GetSquelch { band } => exact_if(matches!(
            response,
            Response::Squelch {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::SetSquelch { band, level } => exact_if(matches!(
            response,
            Response::Squelch {
                band: response_band,
                level: response_level,
            } if response_band == band && response_level == level
        )),
        Command::GetSmeter { band } => exact_if(matches!(
            response,
            Response::Smeter {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::GetOperatingMode { band } => exact_if(matches!(
            response,
            Response::OperatingMode {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::SetOperatingMode { band, mode } => exact_if(matches!(
            response,
            Response::OperatingMode {
                band: response_band,
                mode: response_mode,
            } if response_band == band && response_mode == mode
        )),
        Command::GetFineStep => exact_if(matches!(response, Response::FineStep { .. })),
        Command::GetFineTune => exact_if(matches!(response, Response::FineTune { .. })),
        Command::SetFineTune { enabled } => exact_if(matches!(
            response,
            Response::FineTune {
                enabled: response_enabled,
            } if response_enabled == enabled
        )),
        Command::GetFilterWidth { mode } => exact_if(matches!(
            response,
            Response::FilterWidth { width } if width.mode() == *mode
        )),
        Command::SetFilterWidth { width } => exact_if(matches!(
            response,
            Response::FilterWidth {
                width: response_width,
            } if response_width == width
        )),
        Command::FrequencyUp => exact_if(matches!(response, Response::FrequencyUpAck)),
        Command::FrequencyDown => exact_if(matches!(response, Response::FrequencyDownAck)),
        Command::GetAttenuator { band } => exact_if(matches!(
            response,
            Response::Attenuator {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::SetAttenuator { band, enabled } => exact_if(matches!(
            response,
            Response::Attenuator {
                band: response_band,
                enabled: response_enabled,
            } if response_band == band && response_enabled == enabled
        )),

        // Control
        Command::GetAutoInfo => exact_if(matches!(response, Response::AutoInfo { .. })),
        Command::SetAutoInfo { enabled } => match response {
            Response::AutoInfo {
                enabled: response_enabled,
            } if response_enabled == enabled => ResponseCorrelation::Exact,
            Response::AutoInfoAck => ResponseCorrelation::Acknowledged,
            _ => ResponseCorrelation::Mismatch,
        },
        Command::GetBusy { band } => exact_if(matches!(
            response,
            Response::Busy {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::GetBandMode => exact_if(matches!(response, Response::BandMode { .. })),
        Command::SetBandMode { mode } => exact_if(matches!(
            response,
            Response::BandMode {
                mode: response_mode,
            } if response_mode == mode
        )),
        Command::Receive => exact_if(matches!(response, Response::ReceiveAck)),
        Command::Transmit => exact_if(matches!(response, Response::TransmitAck)),
        Command::GetBacklightControl => {
            exact_if(matches!(response, Response::BacklightControl { .. }))
        }
        Command::SetBacklightControl { mode } => exact_if(matches!(
            response,
            Response::BacklightControl {
                mode: response_mode,
            } if response_mode == mode
        )),
        Command::GetUsbAudioOutput => exact_if(matches!(response, Response::UsbAudioOutput { .. })),
        Command::SetUsbAudioOutput { output } => exact_if(matches!(
            response,
            Response::UsbAudioOutput {
                output: response_output,
            } if response_output == output
        )),
        Command::GetBatteryLevel => exact_if(matches!(response, Response::BatteryLevel { .. })),
        Command::GetVoxDelay => exact_if(matches!(response, Response::VoxDelay { .. })),
        Command::SetVoxDelay { delay } => exact_if(matches!(
            response,
            Response::VoxDelay {
                delay: response_delay,
            } if response_delay == delay
        )),
        Command::GetVoxGain => exact_if(matches!(response, Response::VoxGain { .. })),
        Command::SetVoxGain { gain } => exact_if(matches!(
            response,
            Response::VoxGain {
                gain: response_gain,
            } if response_gain == gain
        )),
        Command::GetVox => exact_if(matches!(response, Response::Vox { .. })),
        Command::SetVox { enabled } => exact_if(matches!(
            response,
            Response::Vox {
                enabled: response_enabled,
            } if response_enabled == enabled
        )),

        // Memory
        Command::GetCurrentChannel { .. } => {
            exact_if(matches!(response, Response::CurrentChannel { .. }))
        }
        Command::GetMemoryChannel { selector } => exact_if(matches!(
            response,
            Response::MemoryChannel {
                selector: response_selector,
                ..
            } if response_selector == selector
        )),
        Command::RecallMemoryChannel { band, selector } => exact_if(matches!(
            response,
            Response::MemoryRecallAck {
                band: response_band,
                selector: response_selector,
            } if response_band == band && response_selector == selector
        )),

        // TNC, D-STAR, and clock
        Command::GetTncMode => exact_if(matches!(response, Response::TncMode { .. })),
        Command::SetTncMode { mode, data_band } => exact_if(matches!(
            response,
            Response::TncMode {
                mode: response_mode,
                data_band: response_data_band,
            } if response_mode == mode && response_data_band == data_band
        )),
        Command::GetDstarCallsign { slot } => exact_if(matches!(
            response,
            Response::DstarCallsign {
                slot: response_slot,
                ..
            } if response_slot == slot
        )),
        Command::SetDstarCallsign {
            slot,
            callsign,
            suffix,
        } => exact_if(matches!(
            response,
            Response::DstarCallsign {
                slot: response_slot,
                callsign: response_callsign,
                suffix: response_suffix,
            } if response_slot == slot
                && response_callsign == callsign
                && response_suffix == suffix
        )),
        Command::GetRealTimeClock => exact_if(matches!(response, Response::RealTimeClock { .. })),

        // Scan
        Command::GetStepSize { band } => exact_if(matches!(
            response,
            Response::StepSize {
                band: response_band,
                ..
            } if response_band == band
        )),
        Command::SetStepSize { band, step } => exact_if(matches!(
            response,
            Response::StepSize {
                band: response_band,
                step: response_step,
            } if response_band == band && response_step == step
        )),
        Command::GetAntennaInput => exact_if(matches!(response, Response::AntennaInput { .. })),
        Command::SetAntennaInput { input } => exact_if(matches!(
            response,
            Response::AntennaInput {
                input: response_input,
            } if response_input == input
        )),

        // APRS
        Command::GetPacketDataRate => exact_if(matches!(response, Response::PacketDataRate { .. })),
        Command::SetPacketDataRate { data_rate } => exact_if(matches!(
            response,
            Response::PacketDataRate {
                data_rate: response_data_rate,
            } if response_data_rate == data_rate
        )),
        Command::GetSerialInfo => exact_if(matches!(response, Response::SerialInformation(_))),
        Command::GetBeaconMode => exact_if(matches!(response, Response::BeaconMode { .. })),
        Command::SetBeaconMode { mode } => exact_if(matches!(
            response,
            Response::BeaconMode {
                mode: response_mode,
            } if response_mode == mode
        )),
        Command::GetMyPositionSelection => {
            exact_if(matches!(response, Response::MyPositionSelection { .. }))
        }
        Command::SetMyPositionSelection { selection } => exact_if(matches!(
            response,
            Response::MyPositionSelection {
                selection: response_selection,
            } if response_selection == selection
        )),
        Command::GetAprsCallsign => exact_if(matches!(response, Response::AprsCallsign { .. })),
        Command::SetAprsCallsign { callsign } => exact_if(matches!(
            response,
            Response::AprsCallsign {
                callsign: Some(response_callsign),
            } if response_callsign == callsign
        )),
        Command::TransmitAprsBeacon => {
            exact_if(matches!(response, Response::AprsBeaconTransmitAck))
        }

        // D-STAR
        Command::GetDstarSlot => exact_if(matches!(response, Response::DstarSlot { .. })),
        Command::SetDstarSlot { slot } => exact_if(matches!(
            response,
            Response::DstarSlot {
                slot: response_slot,
            } if response_slot == slot
        )),
        Command::GetGateway => exact_if(matches!(response, Response::Gateway { .. })),

        // GPS
        Command::GetGpsSettings => exact_if(matches!(response, Response::GpsSettings { .. })),
        Command::SetGpsSettings { settings } => exact_if(matches!(
            response,
            Response::GpsSettings {
                settings: response_settings,
            } if response_settings == settings
        )),
        Command::GetGpsMode => exact_if(matches!(response, Response::GpsMode { .. })),
        Command::GetGpsSentences => exact_if(matches!(response, Response::GpsSentences { .. })),
        Command::SetGpsSentences { sentences } => exact_if(matches!(
            response,
            Response::GpsSentences {
                sentences: response_sentences,
            } if response_sentences == sentences
        )),

        // Bluetooth, SD, modified-firmware memory read, and radio type
        Command::GetBluetooth => exact_if(matches!(response, Response::Bluetooth { .. })),
        Command::SetBluetooth { enabled } => exact_if(matches!(
            response,
            Response::Bluetooth {
                enabled: response_enabled,
            } if response_enabled == enabled
        )),
        Command::GetSdCard => exact_if(matches!(response, Response::SdCard { .. })),
        Command::ReadMemory { offset, len } => exact_if(matches!(
            response,
            Response::MemoryData {
                offset: response_offset,
                bytes,
            } if response_offset == offset && bytes.len() == usize::from(len.as_bytes())
        )),
        Command::GetRadioType => exact_if(matches!(response, Response::RadioType(_))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Band, GpsSettings, NmeaSentences, SquelchLevel, TuningMode};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn setter_requires_every_echoed_field() -> TestResult {
        let command = Command::SetSquelch {
            band: Band::A,
            level: SquelchLevel::new(4)?,
        };
        let exact = Response::Squelch {
            band: Band::A,
            level: SquelchLevel::new(4)?,
        };
        let wrong_value = Response::Squelch {
            band: Band::A,
            level: SquelchLevel::new(3)?,
        };
        let wrong_band = Response::Squelch {
            band: Band::B,
            level: SquelchLevel::new(4)?,
        };

        assert_eq!(correlate(&command, &exact), ResponseCorrelation::Exact);
        assert_eq!(
            correlate(&command, &wrong_value),
            ResponseCorrelation::Mismatch
        );
        assert_eq!(
            correlate(&command, &wrong_band),
            ResponseCorrelation::Mismatch
        );
        Ok(())
    }

    #[test]
    fn read_requires_its_identifying_band() {
        let command = Command::GetTuningMode { band: Band::A };
        let response = Response::TuningMode {
            band: Band::B,
            mode: TuningMode::Memory,
        };

        assert_eq!(
            correlate(&command, &response),
            ResponseCorrelation::Mismatch
        );
    }

    #[test]
    fn gps_setters_require_the_complete_typed_echo() -> TestResult {
        let requested_settings = GpsSettings::new(true, false);
        assert_eq!(
            correlate(
                &Command::SetGpsSettings {
                    settings: requested_settings,
                },
                &Response::GpsSettings {
                    settings: requested_settings,
                },
            ),
            ResponseCorrelation::Exact
        );
        assert_eq!(
            correlate(
                &Command::SetGpsSettings {
                    settings: requested_settings,
                },
                &Response::GpsSettings {
                    settings: GpsSettings::new(false, false),
                },
            ),
            ResponseCorrelation::Mismatch
        );

        let requested_sentences = NmeaSentences::try_from(0x11)?;
        assert_eq!(
            correlate(
                &Command::SetGpsSentences {
                    sentences: requested_sentences,
                },
                &Response::GpsSentences {
                    sentences: requested_sentences,
                },
            ),
            ResponseCorrelation::Exact
        );
        assert_eq!(
            correlate(
                &Command::SetGpsSentences {
                    sentences: requested_sentences,
                },
                &Response::GpsSentences {
                    sentences: NmeaSentences::all(),
                },
            ),
            ResponseCorrelation::Mismatch
        );
        Ok(())
    }

    #[test]
    fn state_free_auto_info_ack_is_distinguished() {
        assert_eq!(
            correlate(
                &Command::SetAutoInfo { enabled: true },
                &Response::AutoInfoAck,
            ),
            ResponseCorrelation::Acknowledged
        );
        assert!(
            ResponseCorrelation::Acknowledged.completes_command(),
            "the high-level setter performs the required readback"
        );
    }

    #[test]
    fn aprs_beacon_action_accepts_only_its_bare_acknowledgement() {
        assert_eq!(
            correlate(
                &Command::TransmitAprsBeacon,
                &Response::AprsBeaconTransmitAck,
            ),
            ResponseCorrelation::Exact
        );
        assert_eq!(
            correlate(
                &Command::TransmitAprsBeacon,
                &Response::BeaconMode {
                    mode: crate::types::BeaconMode::Manual,
                },
            ),
            ResponseCorrelation::Mismatch
        );
    }
}
