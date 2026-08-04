//! Explicitly insecure plaintext-HTTP fetcher for the XLX directory.
//!
//! The server at
//! `http://xlxapi.rlx.lu/api.php?do=GetReflectorHostname` has no working HTTPS
//! endpoint. Fetches through this module have no confidentiality,
//! authenticity, or integrity: an intermediary can observe or replace the
//! response. Enabling the feature and calling the fetch method are both
//! deliberate opt-ins; shipped workspace applications do neither.

mod fetcher;

pub use fetcher::{InsecurePlaintextXlxDirectoryFetchError, InsecurePlaintextXlxDirectoryFetcher};
