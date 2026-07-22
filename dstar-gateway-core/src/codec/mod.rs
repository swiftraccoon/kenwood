//! Wire-format codecs for `DPlus`, `DExtra`, and `DCS`.
//!
//! Each protocol has its own submodule with an identical six-file shape:
//! `mod.rs`, `consts.rs`, `packet.rs`, `encode.rs`, `decode.rs`, `error.rs`.
//! `DPlus` adds a seventh, `auth.rs`; `DExtra` and `DCS` have no auth flow.

pub mod dcs;
pub mod dextra;
pub mod dplus;
