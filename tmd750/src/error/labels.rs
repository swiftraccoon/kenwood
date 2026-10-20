//! Closed command, field and exchange labels used by typed failures.

// Each label has one diagnostic spelling shared by its value and Display.
macro_rules! labels {
    ($(#[$doc:meta])* $name:ident { $($(#[$variant_doc:meta])* $variant:ident => $label:literal,)* }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$variant_doc])* $variant,)*
        }

        impl $name {
            /// Stable label used in diagnostic output.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $label,)* }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.pad(self.as_str())
            }
        }
    };
}

labels! {
    /// A known command whose reply or exchange failed.
    CommandName {
        /// Radio identity.
        Id => "ID",
        /// Firmware identity.
        Fv => "FV",
        /// Radio type.
        Ty => "TY",
        /// Serial information.
        Ae => "AE",
        /// Power state.
        Ps => "PS",
        /// Real-time clock.
        Rt => "RT",
        /// Operating mode.
        Md => "MD",
        /// DV Gateway mode.
        Gw => "GW",
        /// Frequency.
        Fq => "FQ",
        /// VFO channel record.
        Fo => "FO",
        /// Power level.
        Pc => "PC",
        /// Tuning mode.
        Vm => "VM",
        /// Squelch level.
        Sq => "SQ",
        /// Signal-meter reading.
        Sm => "SM",
        /// Busy state.
        By => "BY",
        /// Attenuator state.
        Ra => "RA",
        /// Step size.
        Sf => "SF",
        /// AM high-cut filter.
        Sh => "SH",
        /// Frequency step up.
        Up => "UP",
        /// Frequency step down.
        Dw => "DW",
        /// Selected memory channel.
        Mr => "MR",
        /// Memory channel record.
        Me => "ME",
        /// D-STAR callsign entry.
        Dc => "DC",
        /// Band roles.
        Bc => "BC",
        /// Band display.
        Dl => "DL",
        /// D-STAR callsign slot.
        Ds => "DS",
        /// Backlight control.
        Lc => "LC",
        /// My Position selection.
        Ms => "MS",
        /// Packet data rate.
        As => "AS",
        /// Beacon method.
        Pt => "PT",
        /// TNC mode and data band.
        Tn => "TN",
        /// VOX delay.
        Vd => "VD",
        /// VOX gain.
        Vg => "VG",
        /// VOX state.
        Vx => "VX",
        /// GPS state and PC output.
        Gp => "GP",
        /// GPS sentence selection.
        Gs => "GS",
        /// Bluetooth state.
        Bt => "BT",
        /// APRS callsign.
        Cs => "CS",
        /// Transmitter key-up.
        Tx => "TX",
        /// Transmitter release.
        Rx => "RX",
        /// APRS beacon transmission.
        Be => "BE",
        /// Five-byte programming header.
        McpHeader => "MCP header",
    }
}

labels! {
    /// A known component of a parsed command reply.
    ReplyField {
        /// Band index.
        Band => "band",
        /// Operating, tuning or gateway mode.
        Mode => "mode",
        /// Level value.
        Level => "level",
        /// Firmware version.
        Version => "version",
        /// Radio-type payload.
        RadioType => "radio_type",
        /// Serial information.
        SerialInformation => "serial_information",
        /// Power state.
        Power => "power",
        /// Clock components.
        Clock => "clock",
        /// Frequency in hertz.
        Frequency => "frequency",
        /// Receive frequency.
        ReceiveFrequency => "receive_frequency",
        /// Transmit offset or split frequency.
        Transmit => "transmit",
        /// Receive step.
        ReceiveStep => "receive_step",
        /// Transmit step.
        TransmitStep => "transmit_step",
        /// Tone flag.
        Tone => "tone",
        /// CTCSS flag.
        Ctcss => "ctcss",
        /// DCS flag.
        Dcs => "dcs",
        /// Combined tone mode.
        ToneMode => "tone_mode",
        /// Cross-tone selection.
        CrossTone => "cross_tone",
        /// Reverse state.
        Reverse => "reverse",
        /// Repeater shift.
        Shift => "shift",
        /// Tone table code.
        ToneCode => "tone_code",
        /// CTCSS table code.
        CtcssCode => "ctcss_code",
        /// DCS table code.
        DcsCode => "dcs_code",
        /// UR callsign.
        UrCall => "ur_call",
        /// Digital squelch selection.
        DigitalSquelch => "digital_squelch",
        /// Digital code.
        DigitalCode => "digital_code",
        /// Logical channel or absolute page address.
        Address => "address",
        /// Split flag.
        Split => "split",
        /// Scan lockout flag.
        ScanLockout => "scan_lockout",
        /// Signal-meter reading.
        Reading => "reading",
        /// Busy state.
        Busy => "busy",
        /// Enabled state.
        Enabled => "enabled",
        /// Frequency step.
        Step => "step",
        /// Filter index.
        Filter => "filter",
        /// High-cut selection.
        Cut => "cut",
        /// Complete acknowledgement payload.
        Payload => "payload",
        /// Current memory selector.
        Selector => "selector",
        /// D-STAR slot.
        Slot => "slot",
        /// Callsign text.
        Callsign => "callsign",
        /// Display selection.
        Display => "display",
        /// Backlight control.
        Control => "control",
        /// Position selection.
        Selection => "selection",
        /// Packet data rate.
        Rate => "rate",
        /// Beacon method.
        Method => "method",
        /// VOX delay.
        Delay => "delay",
        /// VOX gain.
        Gain => "gain",
        /// VOX state.
        State => "state",
        /// GPS state.
        Gps => "gps",
        /// GPS PC output.
        PcOutput => "pc_output",
        /// GGA selection.
        Gga => "gga",
        /// GLL selection.
        Gll => "gll",
        /// GSA selection.
        Gsa => "gsa",
        /// GSV selection.
        Gsv => "gsv",
        /// RMC selection.
        Rmc => "rmc",
        /// VTG selection.
        Vtg => "vtg",
        /// Complete programming page.
        Page => "page",
    }
}

labels! {
    /// A bounded setting whose numeric constructor rejected a value.
    SettingName {
        /// Regular channel number.
        MemoryChannel => "memory channel",
        /// Program-scan pair index.
        ProgramScanIndex => "program scan index",
        /// Memory group number.
        MemoryGroup => "memory group",
        /// Weather channel number.
        WeatherChannel => "weather channel",
        /// VFO step segment.
        StepSegment => "step segment",
        /// Terminal MY callsign index.
        TerminalMyCallsignIndex => "Terminal MY callsign index",
        /// Selected MY callsign entry.
        MyCallsignSelection => "MY callsign selection",
        /// RF power level.
        PowerLevel => "power level",
        /// Squelch level.
        SquelchLevel => "squelch level",
        /// AM high-cut setting.
        AmHighCut => "AM high cut",
        /// VOX delay.
        VoxDelay => "VOX delay",
        /// VOX gain.
        VoxGain => "VOX gain",
        /// Backlight control.
        BacklightControl => "backlight control",
        /// Single/dual band display.
        BandDisplay => "band display",
        /// My Position selection.
        MyPositionSelection => "my position selection",
        /// D-STAR MY callsign slot.
        DstarCallsignSlot => "D-STAR callsign slot",
        /// Packet data rate.
        PacketDataRate => "packet data rate",
        /// Beacon method.
        BeaconMethod => "beacon method",
        /// Gregorian clock year.
        ClockYear => "clock year",
        /// Gregorian clock month.
        ClockMonth => "clock month",
        /// Gregorian clock day.
        ClockDay => "clock day",
        /// Clock hour.
        ClockHour => "clock hour",
        /// Clock minute.
        ClockMinute => "clock minute",
        /// Clock second.
        ClockSecond => "clock second",
        /// VFO/memory/DR tuning mode.
        TuningMode => "tuning mode",
        /// Repeater shift direction.
        ShiftDirection => "shift direction",
        /// Physical memory-table index.
        PhysicalChannel => "physical channel",
        /// Stored route field.
        RouteField => "route field",
        /// Stored band code.
        BandCode => "band code",
        /// Stored scan lockout flag.
        ScanLockout => "scan lockout flag",
        /// Tone table index.
        ToneCode => "tone code",
        /// DCS table index.
        DcsCode => "DCS code",
        /// Cross-tone field.
        CrossTone => "cross tone field",
        /// Digital squelch selection.
        DigitalSquelch => "digital squelch",
        /// Digital code.
        DigitalCode => "digital code",
        /// Coordinate minute component.
        PositionMinutes => "position minutes",
        /// Coordinate fractional minute component.
        PositionMinuteFraction => "position minute fraction",
        /// Latitude degree component.
        LatitudeDegrees => "latitude degrees",
        /// Longitude degree component.
        LongitudeDegrees => "longitude degrees",
    }
}

labels! {
    /// The exchange stage awaiting a programming acknowledgement.
    AckStage {
        /// End of programming mode.
        Exit => "MCP exit",
        /// Completion of a page read.
        PageRead => "MCP page read",
        /// Completion of a page write.
        PageWrite => "MCP page write",
        /// Completion of a guarded fixed-text page write.
        TextFieldWrite => "text field page write",
    }
}

/// A bounded I/O operation whose deadline elapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeoutOperation {
    /// A known CAT command's complete reply allowance.
    Command(CommandName),
    /// Writing a complete transport buffer.
    SerialWrite,
    /// Programming-mode entry reply.
    ProgrammingEntry,
    /// A five-byte programming response header.
    McpReadHeader,
    /// A programming page payload.
    McpPageData,
    /// One uniform-fill byte.
    McpFillByte,
    /// A programming acknowledgement.
    Acknowledgement(AckStage),
}

impl std::fmt::Display for TimeoutOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(command) => command.fmt(formatter),
            Self::SerialWrite => formatter.pad("serial write"),
            Self::ProgrammingEntry => formatter.pad("programming mode entry"),
            Self::McpReadHeader => formatter.pad("MCP read header"),
            Self::McpPageData => formatter.pad("MCP page data"),
            Self::McpFillByte => formatter.pad("MCP fill byte"),
            Self::Acknowledgement(stage) => stage.fmt(formatter),
        }
    }
}

labels! {
    /// A programming operation whose partial progress is retained on failure.
    McpWriteOperation {
        /// Whole-page guarded compare-and-exchange.
        CompareExchange => "page compare-and-exchange",
    }
}

/// A rejected field's original parser error or fixed grammar rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReplyFieldError {
    /// An unsigned integer parser rejected the field.
    #[error("{0}")]
    Integer(#[from] std::num::ParseIntError),
    /// A model value rejected the parsed field.
    #[error("{0}")]
    Validation(#[from] super::ValidationError),
    /// The decimal spelling had a sign or unnecessary leading zero.
    #[error("noncanonical decimal value {value:?}")]
    NoncanonicalDecimal {
        /// Rejected text.
        value: String,
    },
    /// A fixed-width decimal had another width or a nondigit byte.
    #[error("expected exactly {width} decimal digits, got {value:?}")]
    DecimalWidth {
        /// Required byte count.
        width: usize,
        /// Rejected text.
        value: String,
    },
    /// A boolean field was neither `0` nor `1`.
    #[error("expected 0 or 1, got {value:?}")]
    Boolean {
        /// Rejected text.
        value: String,
    },
    /// The AM filter selector was not the supported index `0`.
    #[error("expected filter index 0, got {value:?}")]
    FilterIndex {
        /// Rejected text.
        value: String,
    },
    /// A bare acknowledgement carried a payload.
    #[error("expected a bare acknowledgement, got {value:?}")]
    UnexpectedPayload {
        /// Rejected complete payload.
        value: String,
    },
    /// A `TX` acknowledgement did not contain `1`.
    #[error("expected transmitter acknowledgement 1, got {value:?}")]
    TransmitPayload {
        /// Rejected complete payload.
        value: String,
    },
    /// The transmit component and split flag describe different storage forms.
    #[error("transmit field and split flag disagree")]
    SplitConflict,
}
