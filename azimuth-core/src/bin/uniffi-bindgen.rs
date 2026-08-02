//! Crate-local `UniFFI` binding generator pinned to this crate's dependency.

use aprs as _;
use async_trait as _;
use ax25_codec as _;
use azimuth_core as _;
use if_dsp as _;
use kenwood_thd75 as _;
use kiss_tnc as _;
use thiserror as _;
use tokio as _;

fn main() {
    uniffi::uniffi_bindgen_main();
}
