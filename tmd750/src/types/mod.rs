//! Validated value types with no I/O.

pub mod address;
pub mod identity;
pub mod mode;

pub use address::{
    Address, IMAGE_LENGTH, IMAGE_LENGTH_U32, PAGE_SIZE, PAGE_SIZE_U32, Page, Region, SLOT_COUNT,
    SLOT_STRIDE, SlotIndex,
};
pub use identity::{FirmwareIdentity, RadioModel, RadioType};
pub use mode::{Band, DvGatewayMode, OperatingMode, SelectableMode};
