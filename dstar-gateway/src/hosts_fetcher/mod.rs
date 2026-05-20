//! HTTP fetcher for the XLX reflector directory.
//!
//! Downloads `http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname`
//! and parses it via [`dstar_gateway_core::hosts::parse_xlx_directory`].

mod fetcher;

pub use fetcher::{FetcherError, HostsFetcher};
