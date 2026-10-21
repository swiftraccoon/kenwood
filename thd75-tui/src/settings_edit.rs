//! Staged single-setting changes with exact whole-page concurrency checks.

use std::fmt;

use kenwood_thd75::memory::{MemoryError, MemoryImage, SettingsValueError, SettingsWriter};
use kenwood_thd75::protocol::programming::PAGE_SIZE;
use kenwood_thd75::radio::programming::McpPageExchange;
use kenwood_thd75::{Error, WritableMcpPage};

/// One typed settings mutation staged against an immutable image snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsEdit {
    exchange: McpPageExchange,
}

impl SettingsEdit {
    /// Run a typed setter on a private candidate and retain its exact page pair.
    ///
    /// The input image remains unchanged on every result. A complete image
    /// clone bounds staging allocation to the model's fixed image length.
    pub(crate) fn stage<F>(image: &MemoryImage, modify: F) -> Result<Option<Self>, EditError>
    where
        F: FnOnce(&mut SettingsWriter<'_>) -> Result<(), SettingsValueError>,
    {
        let mut candidate = image.clone();
        let Some((offset, _value)) = candidate
            .modify_setting(modify)
            .map_err(EditError::Memory)?
        else {
            return Ok(None);
        };
        let start = usize::from(offset) / PAGE_SIZE * PAGE_SIZE;
        let page = WritableMcpPage::new(
            u16::try_from(start / PAGE_SIZE).map_err(|_error| EditError::InvalidPageSpan)?,
        )
        .map_err(EditError::Page)?;
        let expected = page_bytes(image, start)?;
        let replacement = page_bytes(&candidate, start)?;
        Ok(Some(Self {
            exchange: McpPageExchange::new(page, expected, replacement),
        }))
    }

    /// Borrow the construction-validated compare-and-exchange request.
    pub(crate) const fn exchange(&self) -> &McpPageExchange {
        &self.exchange
    }

    /// Commit a verified replacement only to the same retained expected page.
    pub(crate) fn commit(&self, image: &mut MemoryImage) -> Result<(), EditError> {
        let start = usize::from(self.exchange.page().as_raw()) * PAGE_SIZE;
        if &page_bytes(image, start)? != self.exchange.expected() {
            return Err(EditError::SnapshotChanged);
        }
        let bytes = image
            .as_raw_mut()
            .get_mut(start..start + PAGE_SIZE)
            .ok_or(EditError::InvalidPageSpan)?;
        bytes.copy_from_slice(self.exchange.replacement());
        Ok(())
    }
}

fn page_bytes(image: &MemoryImage, start: usize) -> Result<[u8; PAGE_SIZE], EditError> {
    image
        .as_raw()
        .get(start..start + PAGE_SIZE)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(EditError::InvalidPageSpan)
}

/// Settings staging or local snapshot commit failure.
#[derive(Debug)]
pub(crate) enum EditError {
    /// The model rejected the typed mutation.
    Memory(MemoryError),
    /// The resulting MCP address is outside the writable model domain.
    Page(Error),
    /// A complete fixed-width page is unavailable.
    InvalidPageSpan,
    /// A different local snapshot replaced the staged expected page.
    SnapshotChanged,
}

impl fmt::Display for EditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(error) => error.fmt(formatter),
            Self::Page(error) => error.fmt(formatter),
            Self::InvalidPageSpan => {
                formatter.write_str("a complete writable MCP page is unavailable")
            }
            Self::SnapshotChanged => {
                formatter.write_str("the retained MCP snapshot changed before commit")
            }
        }
    }
}

impl std::error::Error for EditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Memory(error) => Some(error),
            Self::Page(error) => Some(error),
            Self::InvalidPageSpan | Self::SnapshotChanged => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SettingsEdit;
    use kenwood_thd75::memory::MemoryImage;
    use kenwood_thd75::protocol::programming::TOTAL_SIZE;

    #[test]
    fn staging_preserves_snapshot_and_commits_only_matching_verified_page()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut image = MemoryImage::from_raw(vec![0; TOTAL_SIZE])?;
        let before = image.as_raw().to_vec();
        let edit = SettingsEdit::stage(&image, |writer| writer.set_key_beep(true))?
            .ok_or("missing edit")?;
        assert_eq!(image.as_raw(), before);
        assert_ne!(edit.exchange().expected(), edit.exchange().replacement());
        edit.commit(&mut image)?;
        assert!(image.settings().key_beep()?);
        let committed = image.as_raw().to_vec();
        assert!(
            edit.commit(&mut image).is_err(),
            "a different expected page was accepted"
        );
        assert_eq!(image.as_raw(), committed);
        Ok(())
    }
}
