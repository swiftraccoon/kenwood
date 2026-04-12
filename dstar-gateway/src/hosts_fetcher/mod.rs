//! HTTP host-list fetcher for Pi-Star reflector lists.
//!
//! Downloads and parses the canonical Pi-Star reflector host files:
//!
//! - `DPlus` (REF): <https://hosts.pistar.uk/hosts/REFHosts.txt>
//! - `DExtra` (XRF): <https://hosts.pistar.uk/hosts/XRFHosts.txt>
//! - `DCS`: <https://hosts.pistar.uk/hosts/DCSHosts.txt>
//!
//! The fetched text is parsed by
//! [`dstar_gateway_core::hosts::HostFile::parse`], which tolerates the
//! slightly different column layouts of each file and uses a per-file
//! default port when the host line omits the port.

mod fetcher;

pub use fetcher::{FetcherError, HostsFetcher};
