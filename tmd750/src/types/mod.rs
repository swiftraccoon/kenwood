//! Validated value types with no I/O.

pub mod address;
pub mod channel;
pub mod frequency;
pub mod identity;
pub mod mode;
pub mod settings;
pub mod signaling;
pub mod stored;
pub mod tuning;

pub use address::{
    Address, IMAGE_LENGTH, IMAGE_LENGTH_U32, PAGE_SIZE, PAGE_SIZE_U32, Page, Region, SLOT_COUNT,
    SLOT_STRIDE, SlotIndex,
};
pub use channel::{CHANNEL_FIELD_COUNT, CatChannelRecord, CatMemoryChannelRecord};
pub use frequency::{Frequency, OffsetFrequency, StepSize, TransmitSetting, WIRE_DIGITS};
pub use identity::{FirmwareIdentity, RadioModel, RadioType};
pub use mode::{Band, DvGatewayMode, OperatingMode, SelectableMode};
pub use settings::{
    AmHighCut, BacklightControl, BandDisplay, BeaconMethod, DstarCallsignEntry, DstarSlot,
    GpsSettings, MyPositionSelection, NmeaSentence, NmeaSentences, PacketDataRate, PowerLevel,
    RealTimeClock, SMeterReading, SerialInformation, SquelchLevel, TncMode, VoxDelay, VoxGain,
    VoxMode,
};
pub use signaling::{
    CrossToneField, DCS_CODES, DcsCode, DigitalCode, DigitalSquelch, TONE_FREQUENCIES_DECIHERTZ,
    ToneCode, ToneMode, UrCallsign,
};
pub use stored::{
    BandCode, CHANNEL_DATA_OFFSET, CHANNEL_FLAGS_OFFSET, CHANNEL_NAME_SIZE, CHANNEL_NAMES_OFFSET,
    CHANNEL_RECORD_SIZE, CHANNELS_PER_PAGE, ChannelName, FLAG_RECORD_SIZE, PHYSICAL_CHANNEL_COUNT,
    PhysicalChannel, RouteField, StoredCallsign, StoredChannel, StoredChannelEntry,
    StoredChannelFlag,
};
pub use tuning::{
    BandControl, CurrentMemorySelector, MemoryChannelAddress, ShiftDirection, TuningMode,
};
