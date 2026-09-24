//! Interactive vocabulary for the typed CAT reads and verified writes.
//!
//! Each word maps to one library accessor. Band-indexed words take an
//! optional `a` or `b` (Band A is the default) before the value; a word
//! without a value reads the setting.

use std::fmt;

use kenwood_tmd750::types::{
    AmHighCut, BacklightControl, BandControl, BandDisplay, BeaconMethod, DstarSlot, Frequency,
    GpsSettings, MemoryChannelAddress, MyPositionSelection, NmeaSentence, NmeaSentences,
    PacketDataRate, PowerLevel, SquelchLevel, StepSize, TuningMode, VoxDelay, VoxGain,
};
use kenwood_tmd750::{Band, Error, Radio};
use kenwood_transport::Transport;

use crate::output;

/// Help text for this vocabulary, one line per word.
pub(crate) const HELP_LINES: &[&str] = &[
    "serial | clock: Read the serial number or the radio's clock.",
    "freq [a|b] [MHz]: Read or tune the VFO (freq b 446.000).",
    "channel [a|b]: Read a band's complete channel record.",
    "power [a|b] [high|medium|low]: Read or select transmit power.",
    "tuning [a|b] [vfo|memory|call|dr]: Read or select the tuning mode.",
    "squelch [a|b] [0-31]: Read or set the squelch level.",
    "smeter [a|b] | busy [a|b]: Read signal strength or squelch state.",
    "att [a|b] [on|off]: Read or switch the attenuator.",
    "step [a|b] [kHz]: Read or select the tuning step, 1 to 100 kHz.",
    "up [a|b] | down [a|b]: Step the control band by its tuning step.",
    "am-cut [3.0|4.5|6.0|7.5]: Read or select the AM high-cut filter.",
    "current [a|b] | recall [a|b] ADDRESS: Memory channel selection.",
    "memory ADDRESS: Read a stored channel (000-999, L00-U49, Pri).",
    "clear ADDRESS: Empty a stored channel; readback must answer N.",
    "bands [CTRL PTT]: Read or select the band roles (bands b a).",
    "display [dual|single]: Read or select the band display.",
    "slot [1-6] | callsign 1-6: Select the MY slot; read one slot.",
    "backlight [0-3]: Read or select the panel lighting value.",
    "position [gps|1-5]: Read or select the APRS position source.",
    "data-rate [1200|9600]: Read or select the packet data speed.",
    "beacon [manual|ptt|auto|smart]: Read or select the beacon method.",
    "tnc | vox: Read the TNC mode or the VOX state (read only).",
    "vox-delay [ms] | vox-gain [0-9]: Read or select VOX settings.",
    "gps [on|off on|off]: Read or select the GPS and its PC output.",
    "sentences [gga,gll,gsa,gsv,rmc,vtg|none]: Read or select NMEA.",
    "bluetooth [on|off]: Read or switch Bluetooth.",
];

/// A read-only query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Read {
    Serial,
    Clock,
    Frequency(Band),
    Channel(Band),
    Power(Band),
    Tuning(Band),
    Squelch(Band),
    Smeter(Band),
    Busy(Band),
    Attenuator(Band),
    Step(Band),
    AmHighCut,
    CurrentChannel(Band),
    Memory(MemoryChannelAddress),
    Callsign(DstarSlot),
    Bands,
    Display,
    Slot,
    Backlight,
    Position,
    DataRate,
    Beacon,
    Tnc,
    Vox,
    Gps,
    Sentences,
    Bluetooth,
}

/// A setting change verified by echo and readback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Write {
    Frequency(Band, Frequency),
    Power(Band, PowerLevel),
    Tuning(Band, TuningMode),
    Squelch(Band, SquelchLevel),
    Attenuator(Band, bool),
    Step(Band, StepSize),
    AmHighCut(AmHighCut),
    Up(Band),
    Down(Band),
    Recall(Band, MemoryChannelAddress),
    ClearMemory(MemoryChannelAddress),
    Bands(BandControl),
    Display(BandDisplay),
    Slot(DstarSlot),
    Backlight(BacklightControl),
    Position(MyPositionSelection),
    DataRate(PacketDataRate),
    Beacon(BeaconMethod),
    VoxDelay(VoxDelay),
    VoxGain(VoxGain),
    Gps(GpsSettings),
    Sentences(NmeaSentences),
    Bluetooth(bool),
}

/// A parsed word of this vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    Read(Read),
    Write(Write),
}

/// Why a line of this vocabulary was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParseError(pub(crate) String);

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

type WordResult = Option<Result<Parsed, String>>;

/// Parse lowercase words; `None` when the first word is not in this vocabulary.
pub(crate) fn parse(words: &[&str]) -> Option<Result<Parsed, ParseError>> {
    let [word, rest @ ..] = words else {
        return None;
    };
    let result = parse_band_word(word, rest)
        .or_else(|| parse_setting_word(word, rest))
        .or_else(|| parse_auxiliary_word(word, rest))?;
    Some(result.map_err(ParseError))
}

/// Words whose setting is indexed by a band.
fn parse_band_word(word: &str, rest: &[&str]) -> WordResult {
    let result = match word {
        "freq" | "frequency" => banded(rest, Read::Frequency, |band, value| {
            Frequency::from_mhz_str(value)
                .map(|frequency| Write::Frequency(band, frequency))
                .map_err(|error| error.to_string())
        }),
        "channel" => banded_read(rest, "channel", Read::Channel),
        "power" => banded(rest, Read::Power, |band, value| {
            let level = match value {
                "high" | "h" => PowerLevel::High,
                "medium" | "mid" | "m" => PowerLevel::Medium,
                "low" | "l" => PowerLevel::Low,
                other => {
                    return Err(format!("power must be high, medium or low, not {other:?}"));
                }
            };
            Ok(Write::Power(band, level))
        }),
        "tuning" => banded(rest, Read::Tuning, |band, value| {
            let mode = match value {
                "vfo" => TuningMode::Vfo,
                "memory" | "mr" => TuningMode::Memory,
                "call" => TuningMode::Call,
                "dr" => TuningMode::DStarRepeater,
                other => {
                    return Err(format!(
                        "tuning mode must be vfo, memory, call or dr, not {other:?}"
                    ));
                }
            };
            Ok(Write::Tuning(band, mode))
        }),
        "squelch" | "sql" => banded(rest, Read::Squelch, |band, value| {
            parse_u8(value, "squelch level")
                .and_then(|raw| SquelchLevel::new(raw).map_err(|error| error.to_string()))
                .map(|level| Write::Squelch(band, level))
        }),
        "smeter" => banded_read(rest, "smeter", Read::Smeter),
        "busy" => banded_read(rest, "busy", Read::Busy),
        "att" | "attenuator" => banded(rest, Read::Attenuator, |band, value| {
            parse_switch(value).map(|enabled| Write::Attenuator(band, enabled))
        }),
        "step" => banded(rest, Read::Step, |band, value| {
            parse_step(value).map(|step| Write::Step(band, step))
        }),
        "up" => banded_write(rest, "up", Write::Up),
        "down" | "dw" => banded_write(rest, "down", Write::Down),
        "current" => banded_read(rest, "current", Read::CurrentChannel),
        "recall" => {
            let (band, rest) = split_band(rest);
            match rest {
                [address] => parse_address(address)
                    .map(|address| Parsed::Write(Write::Recall(band, address))),
                _ => Err("recall takes an optional band and one memory address".to_owned()),
            }
        }
        "memory" | "me" => match rest {
            [address] => parse_address(address).map(|address| Parsed::Read(Read::Memory(address))),
            _ => {
                Err("memory takes exactly one address: 000-999, L00-L49, U00-U49 or Pri".to_owned())
            }
        },
        "clear" => match rest {
            [address] => {
                parse_address(address).map(|address| Parsed::Write(Write::ClearMemory(address)))
            }
            _ => Err("clear takes exactly one memory address".to_owned()),
        },
        _ => return None,
    };
    Some(result)
}

/// Global settings with a single value.
fn parse_setting_word(word: &str, rest: &[&str]) -> WordResult {
    let result = match word {
        "am-cut" => global(rest, Read::AmHighCut, |value| {
            let cut = match value {
                "3.0" | "3" => AmHighCut::Hz3000,
                "4.5" => AmHighCut::Hz4500,
                "6.0" | "6" => AmHighCut::Hz6000,
                "7.5" => AmHighCut::Hz7500,
                other => {
                    return Err(format!(
                        "AM high cut must be 3.0, 4.5, 6.0 or 7.5, not {other:?}"
                    ));
                }
            };
            Ok(Write::AmHighCut(cut))
        }),
        "callsign" => match rest {
            [slot] => parse_slot(slot).map(|slot| Parsed::Read(Read::Callsign(slot))),
            _ => Err("callsign takes exactly one slot number, 1 through 6".to_owned()),
        },
        "bands" => match rest {
            [] => Ok(Parsed::Read(Read::Bands)),
            [control, ptt] => match (parse_band(control), parse_band(ptt)) {
                (Some(control), Some(ptt)) => {
                    Ok(Parsed::Write(Write::Bands(BandControl { control, ptt })))
                }
                _ => Err("bands takes two band letters: control band, then PTT band".to_owned()),
            },
            _ => Err("bands takes no argument or two band letters".to_owned()),
        },
        "display" => global(rest, Read::Display, |value| match value {
            "dual" => Ok(Write::Display(BandDisplay::Dual)),
            "single" => Ok(Write::Display(BandDisplay::Single)),
            other => Err(format!("display must be dual or single, not {other:?}")),
        }),
        "slot" => global(rest, Read::Slot, |value| parse_slot(value).map(Write::Slot)),
        "backlight" => global(rest, Read::Backlight, |value| {
            parse_u8(value, "backlight value")
                .and_then(|raw| BacklightControl::new(raw).map_err(|error| error.to_string()))
                .map(Write::Backlight)
        }),
        "position" => global(rest, Read::Position, |value| {
            let selection = if value == "gps" {
                MyPositionSelection::GPS
            } else {
                parse_u8(value, "position number").and_then(|raw| {
                    MyPositionSelection::new(raw).map_err(|error| error.to_string())
                })?
            };
            Ok(Write::Position(selection))
        }),
        "data-rate" => global(rest, Read::DataRate, |value| match value {
            "1200" => Ok(Write::DataRate(PacketDataRate::Bps1200)),
            "9600" => Ok(Write::DataRate(PacketDataRate::Bps9600)),
            other => Err(format!("data rate must be 1200 or 9600, not {other:?}")),
        }),
        "beacon" => global(rest, Read::Beacon, |value| {
            let method = match value {
                "manual" => BeaconMethod::Manual,
                "ptt" => BeaconMethod::Ptt,
                "auto" => BeaconMethod::Auto,
                "smart" | "smartbeaconing" => BeaconMethod::SmartBeaconing,
                other => {
                    return Err(format!(
                        "beacon method must be manual, ptt, auto or smart, not {other:?}"
                    ));
                }
            };
            Ok(Write::Beacon(method))
        }),
        _ => return None,
    };
    Some(result)
}

/// Identity, TNC, VOX, GPS and Bluetooth words.
fn parse_auxiliary_word(word: &str, rest: &[&str]) -> WordResult {
    let result = match word {
        "serial" => bare(rest, "serial", Read::Serial),
        "clock" => bare(rest, "clock", Read::Clock),
        "tnc" => bare(rest, "tnc", Read::Tnc),
        "vox" => bare(rest, "vox", Read::Vox),
        "vox-delay" => global(rest, Read::Vox, |value| {
            let milliseconds: u16 = value
                .parse()
                .map_err(|_| format!("VOX delay must be a millisecond value, not {value:?}"))?;
            VoxDelay::MILLISECONDS
                .iter()
                .position(|&entry| entry == milliseconds)
                .and_then(|index| u8::try_from(index).ok())
                .and_then(|index| VoxDelay::new(index).ok())
                .map(Write::VoxDelay)
                .ok_or_else(|| {
                    format!(
                        "VOX delay must be one of {} ms",
                        VoxDelay::MILLISECONDS.map(|ms| ms.to_string()).join(", ")
                    )
                })
        }),
        "vox-gain" => global(rest, Read::Vox, |value| {
            parse_u8(value, "VOX gain")
                .and_then(|raw| VoxGain::new(raw).map_err(|error| error.to_string()))
                .map(Write::VoxGain)
        }),
        "gps" => match rest {
            [] => Ok(Parsed::Read(Read::Gps)),
            [receiver, pc_output] => match (parse_switch(receiver), parse_switch(pc_output)) {
                (Ok(gps_enabled), Ok(pc_output)) => Ok(Parsed::Write(Write::Gps(GpsSettings {
                    gps_enabled,
                    pc_output,
                }))),
                (Err(error), _) | (_, Err(error)) => Err(error),
            },
            _ => Err("gps takes no argument or two switches: receiver, then PC output".to_owned()),
        },
        "sentences" => global(rest, Read::Sentences, |value| {
            parse_sentences(value).map(Write::Sentences)
        }),
        "bluetooth" | "bt" => global(rest, Read::Bluetooth, |value| {
            parse_switch(value).map(Write::Bluetooth)
        }),
        _ => return None,
    };
    Some(result)
}

fn bare(rest: &[&str], word: &str, read: Read) -> Result<Parsed, String> {
    if rest.is_empty() {
        Ok(Parsed::Read(read))
    } else {
        Err(format!("{word} takes no argument"))
    }
}

fn split_band<'a>(rest: &'a [&'a str]) -> (Band, &'a [&'a str]) {
    match rest {
        [first, tail @ ..] => parse_band(first).map_or((Band::A, rest), |band| (band, tail)),
        [] => (Band::A, rest),
    }
}

fn banded_read(rest: &[&str], word: &str, read: fn(Band) -> Read) -> Result<Parsed, String> {
    match split_band(rest) {
        (band, []) => Ok(Parsed::Read(read(band))),
        _ => Err(format!("{word} takes only an optional band letter")),
    }
}

fn banded_write(rest: &[&str], word: &str, write: fn(Band) -> Write) -> Result<Parsed, String> {
    match split_band(rest) {
        (band, []) => Ok(Parsed::Write(write(band))),
        _ => Err(format!("{word} takes only an optional band letter")),
    }
}

fn banded(
    rest: &[&str],
    read: fn(Band) -> Read,
    write: impl FnOnce(Band, &str) -> Result<Write, String>,
) -> Result<Parsed, String> {
    match split_band(rest) {
        (band, []) => Ok(Parsed::Read(read(band))),
        (band, [value]) => write(band, value).map(Parsed::Write),
        _ => Err("expected an optional band letter and at most one value".to_owned()),
    }
}

fn global(
    rest: &[&str],
    read: Read,
    write: impl FnOnce(&str) -> Result<Write, String>,
) -> Result<Parsed, String> {
    match rest {
        [] => Ok(Parsed::Read(read)),
        [value] => write(value).map(Parsed::Write),
        _ => Err("expected at most one value".to_owned()),
    }
}

fn parse_band(word: &str) -> Option<Band> {
    match word {
        "a" => Some(Band::A),
        "b" => Some(Band::B),
        _ => None,
    }
}

fn parse_u8(value: &str, what: &str) -> Result<u8, String> {
    value
        .parse()
        .map_err(|_| format!("{what} must be a whole number, not {value:?}"))
}

fn parse_switch(value: &str) -> Result<bool, String> {
    match value {
        "on" | "1" => Ok(true),
        "off" | "0" => Ok(false),
        other => Err(format!("expected on or off, not {other:?}")),
    }
}

fn parse_slot(value: &str) -> Result<DstarSlot, String> {
    parse_u8(value, "slot").and_then(|raw| DstarSlot::new(raw).map_err(|error| error.to_string()))
}

fn parse_step(value: &str) -> Result<StepSize, String> {
    let wanted = value.trim_end_matches("khz").trim_end();
    StepSize::ALL
        .into_iter()
        .find(|step| {
            let label = step.to_string();
            let label = label.trim_end_matches(" kHz");
            label == wanted || format!("{label}.0") == wanted
        })
        .ok_or_else(|| {
            format!(
                "step must be one of {} kHz",
                StepSize::ALL
                    .map(|step| step.to_string().trim_end_matches(" kHz").to_owned())
                    .join(", ")
            )
        })
}

/// Accept the wire spelling in any letter case: `l05`, `u05` and `pri` name
/// the same channels as `L05`, `U05` and `Pri`.
pub(crate) fn parse_address(value: &str) -> Result<MemoryChannelAddress, String> {
    let normalized = match value.as_bytes() {
        [b'l' | b'L', ..] => format!("L{}", value.get(1..).unwrap_or_default()),
        [b'u' | b'U', ..] => format!("U{}", value.get(1..).unwrap_or_default()),
        _ if value.eq_ignore_ascii_case("pri") => "Pri".to_owned(),
        _ => value.to_owned(),
    };
    MemoryChannelAddress::from_wire_str(&normalized).map_err(|error| error.to_string())
}

fn parse_sentences(value: &str) -> Result<NmeaSentences, String> {
    if value == "none" {
        return Ok(NmeaSentences::NONE);
    }
    value
        .split(',')
        .map(|name| match name {
            "gga" => Ok(NmeaSentence::Gga),
            "gll" => Ok(NmeaSentence::Gll),
            "gsa" => Ok(NmeaSentence::Gsa),
            "gsv" => Ok(NmeaSentence::Gsv),
            "rmc" => Ok(NmeaSentence::Rmc),
            "vtg" => Ok(NmeaSentence::Vtg),
            other => Err(format!(
                "sentence must be gga, gll, gsa, gsv, rmc or vtg, not {other:?}"
            )),
        })
        .collect()
}

/// Run one read and print its result.
pub(crate) async fn execute_read<T: Transport>(
    radio: &mut Radio<T>,
    read: Read,
) -> Result<(), Error> {
    let text = match read {
        Read::Serial => format!("Serial number: {}.", radio.get_serial_information().await?),
        Read::Clock => format!("Clock: {}.", radio.get_real_time_clock().await?),
        Read::Frequency(band) => {
            format!(
                "Band {band} frequency: {}.",
                radio.get_frequency(band).await?
            )
        }
        Read::Channel(band) => format!("Band {band}: {}.", radio.get_channel_record(band).await?),
        Read::Power(band) => format!("Band {band} power: {}.", radio.get_power_level(band).await?),
        Read::Tuning(band) => {
            format!(
                "Band {band} tuning mode: {}.",
                radio.get_tuning_mode(band).await?
            )
        }
        Read::Squelch(band) => format!("Band {band} squelch: {}.", radio.get_squelch(band).await?),
        Read::Smeter(band) => format!("Band {band} S-meter: {}.", radio.get_smeter(band).await?),
        Read::Busy(band) => format!(
            "Band {band} squelch {}.",
            if radio.get_busy(band).await? {
                "open"
            } else {
                "closed"
            }
        ),
        Read::Attenuator(band) => format!(
            "Band {band} attenuator: {}.",
            switch(radio.get_attenuator(band).await?)
        ),
        Read::Step(band) => format!("Band {band} step: {}.", radio.get_step_size(band).await?),
        Read::AmHighCut => format!("AM high cut: {}.", radio.get_am_high_cut().await?),
        Read::CurrentChannel(band) => format!(
            "Band {band} memory channel: {}.",
            radio.get_current_channel(band).await?
        ),
        Read::Memory(address) => match radio.get_memory_channel(address).await? {
            Some(record) => format!(
                "Memory {address}: {}{}{}.",
                record.channel,
                if record.split { " split" } else { "" },
                if record.scan_lockout { " lockout" } else { "" }
            ),
            None => format!("Memory {address}: empty."),
        },
        Read::Callsign(slot) => format!("Callsign {}.", radio.get_dstar_callsign(slot).await?),
        Read::Bands => format!("Band roles: {}.", radio.get_band_control().await?),
        Read::Display => format!("Display: {}.", radio.get_band_display().await?),
        Read::Slot => format!("Selected callsign slot: {}.", radio.get_dstar_slot().await?),
        Read::Backlight => format!("Backlight value: {}.", radio.get_backlight_control().await?),
        Read::Position => format!(
            "APRS position source: {}.",
            radio.get_my_position_selection().await?
        ),
        Read::DataRate => format!("Packet data rate: {}.", radio.get_packet_data_rate().await?),
        Read::Beacon => format!("Beacon method: {}.", radio.get_beacon_method().await?),
        Read::Tnc => {
            let (mode, data_band) = radio.get_tnc_mode().await?;
            format!("TNC: {mode}, data band {data_band} (read only).")
        }
        Read::Vox => format!(
            "VOX: {}, delay {}, gain {} (VOX itself is read only).",
            radio.get_vox().await?,
            radio.get_vox_delay().await?,
            radio.get_vox_gain().await?
        ),
        Read::Gps => format!("GPS: {}.", radio.get_gps_settings().await?),
        Read::Sentences => format!("NMEA sentences: {}.", radio.get_gps_sentences().await?),
        Read::Bluetooth => format!("Bluetooth: {}.", switch(radio.get_bluetooth().await?)),
    };
    output::line(format_args!("{text}"));
    Ok(())
}

/// Run one verified write and print the confirmed setting.
pub(crate) async fn execute_write<T: Transport>(
    radio: &mut Radio<T>,
    write: Write,
) -> Result<(), Error> {
    const VERIFIED: &str = "(write and readback verified)";
    let text = match write {
        Write::Frequency(band, frequency) => {
            radio.set_frequency(band, frequency).await?;
            format!("Band {band} frequency: {frequency} {VERIFIED}.")
        }
        Write::Power(band, level) => {
            radio.set_power_level(band, level).await?;
            format!("Band {band} power: {level} {VERIFIED}.")
        }
        Write::Tuning(band, mode) => {
            radio.set_tuning_mode(band, mode).await?;
            format!("Band {band} tuning mode: {mode} {VERIFIED}.")
        }
        Write::Squelch(band, level) => {
            radio.set_squelch(band, level).await?;
            format!("Band {band} squelch: {level} {VERIFIED}.")
        }
        Write::Attenuator(band, enabled) => {
            radio.set_attenuator(band, enabled).await?;
            format!("Band {band} attenuator: {} {VERIFIED}.", switch(enabled))
        }
        Write::Step(band, step) => {
            radio.set_step_size(band, step).await?;
            let frequency = radio.get_frequency(band).await?;
            format!("Band {band} step: {step} {VERIFIED}; frequency now {frequency}.")
        }
        Write::AmHighCut(cut) => {
            radio.set_am_high_cut(cut).await?;
            format!("AM high cut: {cut} {VERIFIED}.")
        }
        Write::Up(band) => {
            let frequency = radio.frequency_up(band).await?;
            format!("Band {band} stepped up: {frequency}.")
        }
        Write::Down(band) => {
            let frequency = radio.frequency_down(band).await?;
            format!("Band {band} stepped down: {frequency}.")
        }
        Write::Recall(band, address) => {
            radio.recall_memory_channel(band, address).await?;
            format!("Band {band} memory channel: {address} {VERIFIED}.")
        }
        Write::ClearMemory(address) => {
            radio.clear_memory_channel(address).await?;
            format!("Memory {address}: empty (clear acknowledged, readback N).")
        }
        Write::Bands(roles) => {
            radio.set_band_control(roles).await?;
            format!("Band roles: {roles} {VERIFIED}.")
        }
        Write::Display(display) => {
            radio.set_band_display(display).await?;
            format!("Display: {display} {VERIFIED}.")
        }
        Write::Slot(slot) => {
            radio.set_dstar_slot(slot).await?;
            format!("Selected callsign slot: {slot} {VERIFIED}.")
        }
        Write::Backlight(control) => {
            radio.set_backlight_control(control).await?;
            format!("Backlight value: {control} {VERIFIED}.")
        }
        Write::Position(selection) => {
            radio.set_my_position_selection(selection).await?;
            format!("APRS position source: {selection} {VERIFIED}.")
        }
        Write::DataRate(rate) => {
            radio.set_packet_data_rate(rate).await?;
            format!("Packet data rate: {rate} {VERIFIED}.")
        }
        Write::Beacon(method) => {
            radio.set_beacon_method(method).await?;
            format!("Beacon method: {method} {VERIFIED}.")
        }
        Write::VoxDelay(delay) => {
            radio.set_vox_delay(delay).await?;
            format!("VOX delay: {delay} {VERIFIED}.")
        }
        Write::VoxGain(gain) => {
            radio.set_vox_gain(gain).await?;
            format!("VOX gain: {gain} {VERIFIED}.")
        }
        Write::Gps(settings) => {
            radio.set_gps_settings(settings).await?;
            format!("GPS: {settings} {VERIFIED}.")
        }
        Write::Sentences(sentences) => {
            radio.set_gps_sentences(sentences).await?;
            format!("NMEA sentences: {sentences} {VERIFIED}.")
        }
        Write::Bluetooth(enabled) => {
            radio.set_bluetooth(enabled).await?;
            format!("Bluetooth: {} {VERIFIED}.", switch(enabled))
        }
    };
    output::line(format_args!("{text}"));
    Ok(())
}

const fn switch(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn parsed(line: &str) -> Result<Parsed, ParseError> {
        let words: Vec<&str> = line.split_whitespace().collect();
        parse(&words)
            .unwrap_or_else(|| Err(ParseError(format!("{line:?} is outside the vocabulary"))))
    }

    #[test]
    fn reads_default_to_band_a_and_accept_band_b() -> TestResult {
        assert_eq!(parsed("freq")?, Parsed::Read(Read::Frequency(Band::A)));
        assert_eq!(parsed("freq b")?, Parsed::Read(Read::Frequency(Band::B)));
        assert_eq!(parsed("power")?, Parsed::Read(Read::Power(Band::A)));
        assert_eq!(parsed("smeter b")?, Parsed::Read(Read::Smeter(Band::B)));
        assert_eq!(parsed("am-cut")?, Parsed::Read(Read::AmHighCut));
        assert_eq!(parsed("bands")?, Parsed::Read(Read::Bands));
        assert_eq!(parsed("gps")?, Parsed::Read(Read::Gps));
        assert_eq!(parsed("clock")?, Parsed::Read(Read::Clock));
        assert_eq!(
            parsed("memory l05")?,
            Parsed::Read(Read::Memory(MemoryChannelAddress::program_scan_lower(5)?))
        );
        assert_eq!(
            parsed("memory pri")?,
            Parsed::Read(Read::Memory(MemoryChannelAddress::Priority))
        );
        assert_eq!(
            parsed("callsign 6")?,
            Parsed::Read(Read::Callsign(DstarSlot::new(6)?))
        );
        assert_eq!(
            parse(&["gateway"]),
            None,
            "other vocabulary stays with main"
        );
        Ok(())
    }

    #[test]
    fn writes_carry_validated_values() -> TestResult {
        assert_eq!(
            parsed("freq b 446.000")?,
            Parsed::Write(Write::Frequency(Band::B, Frequency::new(446_000_000)?))
        );
        assert_eq!(
            parsed("power low")?,
            Parsed::Write(Write::Power(Band::A, PowerLevel::Low))
        );
        assert_eq!(
            parsed("tuning b dr")?,
            Parsed::Write(Write::Tuning(Band::B, TuningMode::DStarRepeater))
        );
        assert_eq!(
            parsed("squelch 31")?,
            Parsed::Write(Write::Squelch(Band::A, SquelchLevel::new(31)?))
        );
        assert_eq!(
            parsed("att b on")?,
            Parsed::Write(Write::Attenuator(Band::B, true))
        );
        assert_eq!(
            parsed("step 6.25")?,
            Parsed::Write(Write::Step(Band::A, StepSize::Hz6250))
        );
        assert_eq!(
            parsed("step b 100")?,
            Parsed::Write(Write::Step(Band::B, StepSize::Hz100000))
        );
        assert_eq!(parsed("up")?, Parsed::Write(Write::Up(Band::A)));
        assert_eq!(parsed("down b")?, Parsed::Write(Write::Down(Band::B)));
        assert_eq!(
            parsed("am-cut 7.5")?,
            Parsed::Write(Write::AmHighCut(AmHighCut::Hz7500))
        );
        Ok(())
    }

    #[test]
    fn global_writes_carry_validated_values() -> TestResult {
        assert_eq!(
            parsed("recall b 021")?,
            Parsed::Write(Write::Recall(Band::B, MemoryChannelAddress::regular(21)?))
        );
        assert_eq!(
            parsed("clear u49")?,
            Parsed::Write(Write::ClearMemory(
                MemoryChannelAddress::program_scan_upper(49)?
            ))
        );
        assert_eq!(
            parsed("bands b a")?,
            Parsed::Write(Write::Bands(BandControl {
                control: Band::B,
                ptt: Band::A
            }))
        );
        assert_eq!(
            parsed("display single")?,
            Parsed::Write(Write::Display(BandDisplay::Single))
        );
        assert_eq!(
            parsed("slot 2")?,
            Parsed::Write(Write::Slot(DstarSlot::new(2)?))
        );
        assert_eq!(
            parsed("backlight 0")?,
            Parsed::Write(Write::Backlight(BacklightControl::new(0)?))
        );
        assert_eq!(
            parsed("position gps")?,
            Parsed::Write(Write::Position(MyPositionSelection::GPS))
        );
        assert_eq!(
            parsed("data-rate 9600")?,
            Parsed::Write(Write::DataRate(PacketDataRate::Bps9600))
        );
        assert_eq!(
            parsed("beacon smart")?,
            Parsed::Write(Write::Beacon(BeaconMethod::SmartBeaconing))
        );
        assert_eq!(
            parsed("vox-delay 3000")?,
            Parsed::Write(Write::VoxDelay(VoxDelay::new(6)?))
        );
        assert_eq!(
            parsed("vox-gain 9")?,
            Parsed::Write(Write::VoxGain(VoxGain::new(9)?))
        );
        assert_eq!(
            parsed("gps on off")?,
            Parsed::Write(Write::Gps(GpsSettings {
                gps_enabled: true,
                pc_output: false
            }))
        );
        assert_eq!(
            parsed("sentences gga,rmc")?,
            Parsed::Write(Write::Sentences(NmeaSentences::from_flags([
                true, false, false, false, true, false
            ])))
        );
        assert_eq!(
            parsed("sentences none")?,
            Parsed::Write(Write::Sentences(NmeaSentences::NONE))
        );
        assert_eq!(
            parsed("bluetooth off")?,
            Parsed::Write(Write::Bluetooth(false))
        );
        Ok(())
    }

    #[test]
    fn out_of_domain_values_are_refused_before_any_io() {
        for line in [
            "squelch 32",
            "power extra",
            "step 9",
            "step b 4 5",
            "am-cut 8",
            "slot 7",
            "backlight 4",
            "position 6",
            "data-rate 4800",
            "beacon periodic",
            "vox-delay 100",
            "vox-gain 10",
            "gps on",
            "sentences gga;rmc",
            "bands c a",
            "memory 1000",
            "memory",
            "clear",
            "clear 1000",
            "callsign 0",
            "recall",
            "freq 600",
            "tnc a",
            "clock now",
        ] {
            assert!(parsed(line).is_err(), "{line:?} must be refused");
        }
    }

    #[tokio::test]
    async fn reads_send_one_query_each() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"FQ 1\r", b"FQ 1,0223000000\r");
        mock.expect(b"SH 0\r", b"SH 0,2\r");
        mock.expect(b"ME 021\r", b"N\r");
        let mut radio = Radio::new(mock);
        execute_read(&mut radio, Read::Frequency(Band::B)).await?;
        execute_read(&mut radio, Read::AmHighCut).await?;
        execute_read(&mut radio, Read::Memory(MemoryChannelAddress::regular(21)?)).await?;
        radio.into_transport().assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn writes_verify_echo_and_readback_after_the_identity_gate() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        mock.expect(b"FV\r", b"FV 1.02\r");
        mock.expect(b"TY\r", b"TY K,2,1\r");
        mock.expect(b"SF 0,5\r", b"SF 0,5\r");
        mock.expect(b"SF 0\r", b"SF 0,5\r");
        mock.expect(b"FQ 0\r", b"FQ 0,0145180000\r");
        mock.expect(b"BC\r", b"BC 0,0\r");
        mock.expect(b"FQ 0\r", b"FQ 0,0145180000\r");
        mock.expect(b"UP\r", b"UP\r");
        mock.expect(b"FQ 0\r", b"FQ 0,0145190000\r");
        let mut radio = Radio::new(mock);
        execute_write(&mut radio, Write::Step(Band::A, StepSize::Hz10000)).await?;
        execute_write(&mut radio, Write::Up(Band::A)).await?;
        radio.into_transport().assert_complete();
        Ok(())
    }
}
