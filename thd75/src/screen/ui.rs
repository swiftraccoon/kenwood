//! Deterministic TH-D75 V1.03 screen-selection semantics.
//!
//! The stock V1.03 menu UI marks the active row with one exact RGB565 color.
//! Detecting that color in the authenticated framebuffer provides a stronger
//! selected-value oracle than OCR alone: OCR supplies the text, while the
//! pixels prove which row the radio highlighted.

use super::{SCREEN_HEIGHT, SCREEN_WIDTH, ScreenFrame};

/// Exact RGB565 value used by V1.03 for a selected menu row.
///
/// Converting this value with [`ScreenFrame::to_rgb888`] yields
/// `(128, 208, 248)`.
pub const V103_SELECTION_RGB565: u16 = 0x869F;

/// Minimum matching pixels required for one row to count as selected.
///
/// Menu selection bars span nearly the complete 240-pixel width. Keeping the
/// threshold well below that width tolerates glyphs drawn over the bar while
/// rejecting isolated palette-colored icons.
pub const V103_SELECTION_MIN_PIXELS_PER_ROW: usize = 80;

const V103_CHECKBOX_LEFT: usize = 7;
const V103_CHECKBOX_RIGHT_EXCLUSIVE: usize = 25;
const V103_FIRST_ROW_CENTER: usize = 32;
const V103_ROW_PITCH: usize = 24;
const V103_CHECKBOX_HALF_HEIGHT: usize = 9;

/// Visible checked state of one V1.03 checkbox-list row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckboxState {
    /// A red check glyph is present inside the checkbox.
    Checked,
    /// A checkbox outline is present without a red check glyph.
    Unchecked,
}

/// Read one of the six visible V1.03 checkbox rows from exact pixels.
///
/// `visible_slot` is zero-based and describes the row's current on-screen
/// position, not its logical index in a scrollable list. Unknown pixels and
/// slots below the six-row viewport return `None` rather than guessing.
#[must_use]
pub fn v103_checkbox_state(frame: &ScreenFrame, visible_slot: usize) -> Option<CheckboxState> {
    if visible_slot >= 6 {
        return None;
    }
    let center = V103_FIRST_ROW_CENTER.checked_add(V103_ROW_PITCH.checked_mul(visible_slot)?)?;
    let top = center.checked_sub(V103_CHECKBOX_HALF_HEIGHT)?;
    let bottom = center.checked_add(V103_CHECKBOX_HALF_HEIGHT)?;
    if bottom > SCREEN_HEIGHT {
        return None;
    }

    let mut red = 0_usize;
    let mut gray = 0_usize;
    for y in top..bottom {
        for x in V103_CHECKBOX_LEFT..V103_CHECKBOX_RIGHT_EXCLUSIVE {
            let pixel = frame.pixel(x, y).ok()?;
            let red5 = (pixel >> 11) & 0x1F;
            let green6 = (pixel >> 5) & 0x3F;
            let blue5 = pixel & 0x1F;
            if red5 >= 24 && green6 <= 18 && blue5 <= 8 {
                red = red.saturating_add(1);
            }
            let gray_distance = red5.abs_diff(blue5);
            let green_distance = green6.abs_diff(red5.saturating_mul(2));
            if (7..=27).contains(&red5) && gray_distance <= 3 && green_distance <= 6 {
                gray = gray.saturating_add(1);
            }
        }
    }
    if red >= 3 {
        Some(CheckboxState::Checked)
    } else if gray >= 5 {
        Some(CheckboxState::Unchecked)
    } else {
        None
    }
}

/// Return the visible slot and state of the selected V1.03 checkbox row.
///
/// This combines the exact selection-band and checkbox-pixel oracles. It
/// fails closed unless the frame contains exactly one selection band, that
/// band contains exactly one of the six fixed row centers, and the checkbox
/// at that center has a recognized checked or unchecked glyph. The returned
/// slot is its current on-screen position; it is not the row's logical index
/// in a scrollable list.
#[must_use]
pub fn v103_selected_checkbox(frame: &ScreenFrame) -> Option<(usize, CheckboxState)> {
    let bands = v103_selection_bands(frame);
    let [band] = bands.as_slice() else {
        return None;
    };
    let mut slots = (0..6).filter(|slot| {
        let center = V103_FIRST_ROW_CENTER + V103_ROW_PITCH * slot;
        (band.top..band.bottom_exclusive).contains(&center)
    });
    let slot = slots.next()?;
    if slots.next().is_some() {
        return None;
    }
    Some((slot, v103_checkbox_state(frame, slot)?))
}

/// One contiguous horizontal selection band in framebuffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionBand {
    top: usize,
    bottom_exclusive: usize,
}

impl SelectionBand {
    /// First included framebuffer row.
    #[must_use]
    pub const fn top(self) -> usize {
        self.top
    }

    /// First framebuffer row below the band.
    #[must_use]
    pub const fn bottom_exclusive(self) -> usize {
        self.bottom_exclusive
    }

    /// Number of framebuffer rows in the band.
    #[must_use]
    pub const fn height(self) -> usize {
        self.bottom_exclusive - self.top
    }

    /// Whether a normalized top-origin vertical center lies in this band.
    #[must_use]
    pub fn contains_normalized_y(self, y: f32) -> bool {
        if !y.is_finite() || !(0.0..=1.0).contains(&y) {
            return false;
        }
        let (Ok(top), Ok(bottom_exclusive)) = (
            u16::try_from(self.top),
            u16::try_from(self.bottom_exclusive),
        ) else {
            return false;
        };
        let pixel_y = y * 180.0;
        pixel_y >= f32::from(top) && pixel_y < f32::from(bottom_exclusive)
    }
}

/// Find every exact V1.03 selected-row band in a screen frame.
///
/// The result is ordered from top to bottom. Ordinary menu screens contain
/// one band; screens with no selected row return an empty vector.
#[must_use]
pub fn v103_selection_bands(frame: &ScreenFrame) -> Vec<SelectionBand> {
    let mut selected_rows = [false; SCREEN_HEIGHT];
    for (y, row) in frame.rgb565_le().chunks_exact(SCREEN_WIDTH * 2).enumerate() {
        let matches = row
            .chunks_exact(2)
            .filter(|pixel| {
                <[u8; 2]>::try_from(*pixel)
                    .map(u16::from_le_bytes)
                    .is_ok_and(|value| value == V103_SELECTION_RGB565)
            })
            .count();
        if let Some(selected) = selected_rows.get_mut(y) {
            *selected = matches >= V103_SELECTION_MIN_PIXELS_PER_ROW;
        }
    }

    let mut bands = Vec::new();
    let mut start = None;
    for (row, selected) in selected_rows
        .into_iter()
        .chain(std::iter::once(false))
        .enumerate()
    {
        match (start, selected) {
            (None, true) => start = Some(row),
            (Some(top), false) => {
                bands.push(SelectionBand {
                    top,
                    bottom_exclusive: row,
                });
                start = None;
            }
            _ => {}
        }
    }
    bands
}

#[cfg(any(target_os = "macos", doc))]
/// Return OCR observations whose vertical centers lie in a selected band.
#[must_use]
pub fn selected_text<'observation>(
    observations: &'observation [super::vision::TextObservation],
    bands: &[SelectionBand],
) -> Vec<&'observation super::vision::TextObservation> {
    observations
        .iter()
        .filter(|observation| {
            let bounds = observation.bounds();
            let center_y = bounds.y() + bounds.height() / 2.0;
            bands
                .iter()
                .any(|band| band.contains_normalized_y(center_y))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        CheckboxState, SCREEN_HEIGHT, SCREEN_WIDTH, SelectionBand, V103_CHECKBOX_LEFT,
        V103_FIRST_ROW_CENTER, V103_ROW_PITCH, V103_SELECTION_MIN_PIXELS_PER_ROW,
        V103_SELECTION_RGB565, v103_checkbox_state, v103_selected_checkbox, v103_selection_bands,
    };
    use crate::screen::{SCREEN_BYTES, ScreenFrame};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn frame_with_selection(top: usize, bottom_exclusive: usize) -> TestResult<ScreenFrame> {
        let mut bytes = vec![0_u8; SCREEN_BYTES];
        for y in top..bottom_exclusive {
            for x in 0..V103_SELECTION_MIN_PIXELS_PER_ROW {
                let offset = (y * SCREEN_WIDTH + x) * 2;
                bytes
                    .get_mut(offset..offset + 2)
                    .ok_or("synthetic selection pixel is out of range")?
                    .copy_from_slice(&V103_SELECTION_RGB565.to_le_bytes());
            }
        }
        Ok(ScreenFrame::from_rgb565_le(bytes)?)
    }

    #[test]
    fn detects_one_exact_contiguous_selection_band() -> TestResult {
        let frame = frame_with_selection(23, 43)?;
        assert_eq!(
            v103_selection_bands(&frame),
            vec![SelectionBand {
                top: 23,
                bottom_exclusive: 43,
            }]
        );
        Ok(())
    }

    #[test]
    fn rejects_rows_below_the_minimum_width() -> TestResult {
        let mut bytes = vec![0_u8; SCREEN_BYTES];
        for x in 0..V103_SELECTION_MIN_PIXELS_PER_ROW - 1 {
            let offset = x * 2;
            bytes
                .get_mut(offset..offset + 2)
                .ok_or("synthetic palette pixel is out of range")?
                .copy_from_slice(&V103_SELECTION_RGB565.to_le_bytes());
        }
        let frame = ScreenFrame::from_rgb565_le(bytes)?;
        assert!(v103_selection_bands(&frame).is_empty());
        Ok(())
    }

    #[test]
    fn normalized_centers_use_top_origin_and_half_open_edges() {
        let band = SelectionBand {
            top: 18,
            bottom_exclusive: 36,
        };
        assert!(band.contains_normalized_y(0.1));
        assert!(!band.contains_normalized_y(0.2));
        assert!(!band.contains_normalized_y(-0.1));
        assert!(!band.contains_normalized_y(f32::NAN));
        assert_eq!(band.height(), 18);
        assert!(band.bottom_exclusive() <= SCREEN_HEIGHT);
    }

    #[test]
    fn checkbox_rows_distinguish_checked_unchecked_and_unknown() -> TestResult {
        let mut bytes = vec![0_u8; SCREEN_BYTES];
        for (slot, checked) in [(0_usize, true), (1, false)] {
            let center = V103_FIRST_ROW_CENTER + V103_ROW_PITCH * slot;
            for y in center - 7..=center + 7 {
                for x in V103_CHECKBOX_LEFT + 1..V103_CHECKBOX_LEFT + 14 {
                    if x == V103_CHECKBOX_LEFT + 1
                        || x == V103_CHECKBOX_LEFT + 13
                        || y == center - 7
                        || y == center + 7
                    {
                        let offset = (y * SCREEN_WIDTH + x) * 2;
                        bytes
                            .get_mut(offset..offset + 2)
                            .ok_or("synthetic checkbox outline is out of range")?
                            .copy_from_slice(&0x7BEF_u16.to_le_bytes());
                    }
                }
            }
            if checked {
                for diagonal in 0..5 {
                    let x = V103_CHECKBOX_LEFT + 4 + diagonal;
                    let y = center + diagonal / 2;
                    let offset = (y * SCREEN_WIDTH + x) * 2;
                    bytes
                        .get_mut(offset..offset + 2)
                        .ok_or("synthetic checkbox check is out of range")?
                        .copy_from_slice(&0xF800_u16.to_le_bytes());
                }
            }
        }
        let frame = ScreenFrame::from_rgb565_le(bytes)?;
        assert_eq!(v103_checkbox_state(&frame, 0), Some(CheckboxState::Checked));
        assert_eq!(
            v103_checkbox_state(&frame, 1),
            Some(CheckboxState::Unchecked)
        );
        assert_eq!(v103_checkbox_state(&frame, 2), None);
        assert_eq!(v103_checkbox_state(&frame, 6), None);
        Ok(())
    }

    #[test]
    fn selected_checkbox_requires_one_exact_selected_row() -> TestResult {
        let mut bytes = vec![0_u8; SCREEN_BYTES];
        let slot = 5;
        let center = V103_FIRST_ROW_CENTER + V103_ROW_PITCH * slot;
        for y in center - 8..center + 8 {
            for x in 0..SCREEN_WIDTH {
                let offset = (y * SCREEN_WIDTH + x) * 2;
                bytes
                    .get_mut(offset..offset + 2)
                    .ok_or("synthetic selection pixel is out of range")?
                    .copy_from_slice(&V103_SELECTION_RGB565.to_le_bytes());
            }
        }
        for y in center - 7..=center + 7 {
            for x in V103_CHECKBOX_LEFT + 1..V103_CHECKBOX_LEFT + 14 {
                if x == V103_CHECKBOX_LEFT + 1
                    || x == V103_CHECKBOX_LEFT + 13
                    || y == center - 7
                    || y == center + 7
                {
                    let offset = (y * SCREEN_WIDTH + x) * 2;
                    bytes
                        .get_mut(offset..offset + 2)
                        .ok_or("synthetic checkbox outline is out of range")?
                        .copy_from_slice(&0x7BEF_u16.to_le_bytes());
                }
            }
        }
        let frame = ScreenFrame::from_rgb565_le(bytes)?;
        assert_eq!(
            v103_selected_checkbox(&frame),
            Some((slot, CheckboxState::Unchecked))
        );

        let blank = ScreenFrame::from_rgb565_le(vec![0_u8; SCREEN_BYTES])?;
        assert_eq!(v103_selected_checkbox(&blank), None);
        Ok(())
    }
}
