// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Bounded HTTP responses shared by harvesting and activity polling.

use std::num::NonZeroUsize;
use std::time::Duration;

/// An HTTP response exceeded its byte bound, could not be allocated, or failed in transit.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// DNS, connection, redirect, deadline or response-stream failure.
    #[error("HTTP fetch: {0}")]
    Http(#[from] reqwest::Error),
    /// Declared or received bytes exceed the selected memory bound.
    #[error("HTTP response exceeds {maximum} bytes")]
    TooLarge {
        /// Caller-selected maximum retained response bytes.
        maximum: NonZeroUsize,
    },
    /// Reserving additional bounded response storage failed.
    #[error("HTTP response allocation: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
}

#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) body: Vec<u8>,
}

pub(crate) fn client(
    user_agent: &str,
    timeout: Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5))
        .pool_max_idle_per_host(1)
        .build()
}

pub(crate) async fn fetch(
    client: &reqwest::Client,
    url: reqwest::Url,
    maximum: NonZeroUsize,
) -> Result<HttpResponse, FetchError> {
    let mut response = client.get(url).send().await?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > maximum.get()))
    {
        return Err(FetchError::TooLarge { maximum });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let length = body
            .len()
            .checked_add(chunk.len())
            .filter(|length| *length <= maximum.get())
            .ok_or(FetchError::TooLarge { maximum })?;
        body.try_reserve(length - body.len())
            .map_err(FetchError::Allocation)?;
        body.extend_from_slice(&chunk);
    }
    Ok(HttpResponse { status, body })
}
