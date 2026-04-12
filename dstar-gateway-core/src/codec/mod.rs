//! Wire-format codecs for `DPlus`, `DExtra`, and `DCS`.
//!
//! Each protocol has its own submodule with identical six-file shape:
//! `mod.rs`, `consts.rs`, `packet.rs`, `encode.rs`, `decode.rs`,
//! `auth.rs` (`DPlus` only — `DExtra` and `DCS` have no auth flow).

pub mod dcs;
pub mod dextra;
pub mod dplus;
