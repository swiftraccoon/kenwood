//! Explicitly insecure plaintext-HTTP fetcher for the XLX directory.
//!
//! The selected URL is
//! `http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname`.
//! Fetches through this module have no transport confidentiality,
//! authenticity, or integrity: an intermediary can observe or replace the
//! response. Enabling the feature and calling the fetch method are both
//! deliberate opt-ins; shipped workspace applications do neither.

pub use fetcher::{InsecurePlaintextXlxDirectoryFetchError, InsecurePlaintextXlxDirectoryFetcher};

mod fetcher;
