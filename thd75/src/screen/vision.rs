//! macOS Vision text recognition for native TH-D75 screen frames.
//!
//! Recognition always consumes [`ScreenFrame::to_rgb888`], so the native
//! bridge receives one deterministic, top-down, tightly packed RGB888 image.
//! Observation bounds use a top-left origin, matching the radio screen rather
//! than Vision's native lower-left coordinate system.

use thiserror::Error;

use super::ScreenFrame;

/// A rectangle in normalized screen coordinates with a top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedBounds {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl NormalizedBounds {
    /// The complete radio screen.
    pub const FULL_SCREEN: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    /// Construct a non-empty rectangle contained by the normalized screen.
    ///
    /// # Errors
    ///
    /// Returns [`BoundsError`] when a component is non-finite, an origin lies
    /// outside `0.0..=1.0`, an extent is not positive, or an edge extends
    /// beyond the screen.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Result<Self, BoundsError> {
        if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
            return Err(BoundsError::NonFinite);
        }
        if x < 0.0 || x > 1.0 || y < 0.0 || y > 1.0 {
            return Err(BoundsError::OriginOutsideScreen);
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(BoundsError::Empty);
        }
        if width > 1.0 - x || height > 1.0 - y {
            return Err(BoundsError::ExtendsOutsideScreen);
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Horizontal position of the left edge.
    #[must_use]
    pub const fn x(self) -> f32 {
        self.x
    }

    /// Vertical position of the top edge.
    #[must_use]
    pub const fn y(self) -> f32 {
        self.y
    }

    /// Rectangle width.
    #[must_use]
    pub const fn width(self) -> f32 {
        self.width
    }

    /// Rectangle height.
    #[must_use]
    pub const fn height(self) -> f32 {
        self.height
    }

    const fn area(self) -> f32 {
        self.width * self.height
    }

    fn coverage_by(self, roi: Self) -> f32 {
        let self_right = self.x + self.width;
        let self_bottom = self.y + self.height;
        let roi_right = roi.x + roi.width;
        let roi_bottom = roi.y + roi.height;
        if self.x >= roi.x
            && self.y >= roi.y
            && self_right <= roi_right
            && self_bottom <= roi_bottom
        {
            return 1.0;
        }

        let left = self.x.max(roi.x);
        let top = self.y.max(roi.y);
        let right = self_right.min(roi_right);
        let bottom = self_bottom.min(roi_bottom);
        let intersection_width = (right - left).max(0.0);
        let intersection_height = (bottom - top).max(0.0);
        (intersection_width * intersection_height) / self.area()
    }
}

/// Why normalized screen bounds could not be constructed.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoundsError {
    /// At least one component was NaN or infinite.
    #[error("normalized bounds contain a non-finite component")]
    NonFinite,
    /// The rectangle origin was outside the normalized screen.
    #[error("normalized bounds origin is outside the screen")]
    OriginOutsideScreen,
    /// Width or height was zero or negative.
    #[error("normalized bounds must have positive width and height")]
    Empty,
    /// The rectangle's right or bottom edge exceeded the normalized screen.
    #[error("normalized bounds extend outside the screen")]
    ExtendsOutsideScreen,
}

/// One owned text result returned by macOS Vision.
#[derive(Debug, Clone, PartialEq)]
pub struct TextObservation {
    text: String,
    confidence: f32,
    bounds: NormalizedBounds,
}

impl TextObservation {
    /// Construct and validate an owned text observation.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError`] for blank text or a confidence value
    /// outside `0.0..=1.0`.
    pub fn new(
        text: impl Into<String>,
        confidence: f32,
        bounds: NormalizedBounds,
    ) -> Result<Self, ObservationError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(ObservationError::BlankText);
        }
        if !valid_threshold(confidence) {
            return Err(ObservationError::InvalidConfidence { confidence });
        }
        Ok(Self {
            text,
            confidence,
            bounds,
        })
    }

    /// Recognized text exactly as returned by Vision.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Vision's confidence score in `0.0..=1.0`.
    #[must_use]
    pub const fn confidence(&self) -> f32 {
        self.confidence
    }

    /// Normalized bounds with a top-left screen origin.
    #[must_use]
    pub const fn bounds(&self) -> NormalizedBounds {
        self.bounds
    }
}

/// Why a text observation could not be constructed.
#[derive(Debug, Error, Clone, PartialEq)]
#[non_exhaustive]
pub enum ObservationError {
    /// The observation contained no visible text.
    #[error("recognized text is blank")]
    BlankText,
    /// Confidence was non-finite or outside `0.0..=1.0`.
    #[error("recognition confidence {confidence} is outside 0.0..=1.0")]
    InvalidConfidence {
        /// Invalid confidence value.
        confidence: f32,
    },
}

/// A macOS Vision recognition failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VisionOcrError {
    /// The RGB888 conversion did not have the fixed screen length.
    #[error("RGB888 screen is {actual} bytes, expected exactly {expected}")]
    InvalidRgbLength {
        /// Observed byte count.
        actual: usize,
        /// Required byte count.
        expected: usize,
    },
    /// Native Vision or CoreGraphics rejected the request.
    #[error("native Vision OCR failed with status {status}: {message}")]
    NativeFailure {
        /// Stable bridge status code.
        status: i32,
        /// Native diagnostic text.
        message: String,
    },
    /// The native bridge produced a text string that was not UTF-8.
    #[error("Vision returned text that is not valid UTF-8")]
    InvalidUtf8,
    /// A native observation violated the safe Rust API's invariants.
    #[error("Vision returned an invalid observation: {reason}")]
    InvalidObservation {
        /// Rejected observation detail.
        reason: String,
    },
    /// Native Vision returned an unreasonable number of observations.
    #[error("Vision returned more than the {limit} observation safety limit")]
    TooManyObservations {
        /// Maximum accepted observation count.
        limit: usize,
    },
    /// Native Vision returned an unreasonably large text candidate.
    #[error("Vision returned a text candidate longer than the {limit}-byte safety limit")]
    TextTooLong {
        /// Maximum accepted candidate byte count.
        limit: usize,
    },
    /// Rust code unexpectedly panicked while processing a native callback.
    #[error("a panic was contained while processing a Vision observation")]
    CallbackPanicked,
}

/// Why strict expected-text validation did not produce one authoritative match.
#[derive(Debug, Error, Clone, PartialEq)]
#[non_exhaustive]
pub enum TextMatchError {
    /// The expected text was blank.
    #[error("expected text must not be blank")]
    BlankExpectedText,
    /// A confidence or ROI-coverage threshold was invalid.
    #[error("{name} threshold {value} is outside 0.0..=1.0")]
    InvalidThreshold {
        /// Name of the invalid threshold.
        name: &'static str,
        /// Invalid value.
        value: f32,
    },
    /// No observation met every strict condition.
    #[error("expected text {expected:?} was not found at the required confidence and ROI")]
    Missing {
        /// Exact expected text.
        expected: String,
    },
    /// More than one observation met every strict condition.
    #[error("expected text {expected:?} matched {matches} observations; exactly one is required")]
    Ambiguous {
        /// Exact expected text.
        expected: String,
        /// Number of qualifying observations.
        matches: usize,
    },
}

/// Require exactly one exact text match at the requested confidence and ROI.
///
/// Matching is case-sensitive and does not normalize whitespace. An
/// observation qualifies when at least `min_roi_coverage` of its own area
/// overlaps `roi`; use [`NormalizedBounds::FULL_SCREEN`] with a coverage of
/// `1.0` to search the entire display.
///
/// # Errors
///
/// Returns [`TextMatchError::Missing`] when no observation qualifies and
/// [`TextMatchError::Ambiguous`] when more than one qualifies. Blank expected
/// text and non-finite or out-of-range thresholds are rejected explicitly.
pub fn require_unique_text<'observation>(
    observations: &'observation [TextObservation],
    expected: &str,
    min_confidence: f32,
    roi: NormalizedBounds,
    min_roi_coverage: f32,
) -> Result<&'observation TextObservation, TextMatchError> {
    if expected.trim().is_empty() {
        return Err(TextMatchError::BlankExpectedText);
    }
    validate_threshold("minimum confidence", min_confidence)?;
    validate_threshold("minimum ROI coverage", min_roi_coverage)?;

    let mut matched = None;
    let mut match_count = 0_usize;
    for observation in observations {
        if observation.text == expected
            && observation.confidence >= min_confidence
            && observation.bounds.coverage_by(roi) >= min_roi_coverage
        {
            match_count = match_count.saturating_add(1);
            if matched.is_none() {
                matched = Some(observation);
            }
        }
    }

    match (matched, match_count) {
        (Some(observation), 1) => Ok(observation),
        (None, 0) => Err(TextMatchError::Missing {
            expected: expected.to_owned(),
        }),
        _ => Err(TextMatchError::Ambiguous {
            expected: expected.to_owned(),
            matches: match_count,
        }),
    }
}

const fn valid_threshold(value: f32) -> bool {
    value.is_finite() && value >= 0.0 && value <= 1.0
}

const fn validate_threshold(name: &'static str, value: f32) -> Result<(), TextMatchError> {
    if valid_threshold(value) {
        Ok(())
    } else {
        Err(TextMatchError::InvalidThreshold { name, value })
    }
}

#[cfg(target_os = "macos")]
impl ScreenFrame {
    /// Recognize all text in this exact screen frame with macOS Vision.
    ///
    /// Results are owned and sorted deterministically from top to bottom, then
    /// left to right. The native bridge uses accurate recognition without
    /// language correction so screen text is not silently rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`VisionOcrError`] when conversion invariants fail, native
    /// Vision rejects the image, or a callback violates the safe observation
    /// invariants.
    pub fn recognize_text(&self) -> Result<Vec<TextObservation>, VisionOcrError> {
        macos::recognize_screen(self)
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "This narrowly scoped module calls the synchronous Objective-C Vision bridge. \
              Every pointer lifetime, size, callback, and unsafe operation is checked and \
              documented locally; the public API remains safe."
)]
mod macos {
    use std::ffi::c_void;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::{slice, str};

    use super::{NormalizedBounds, TextObservation, VisionOcrError};
    use crate::screen::{SCREEN_HEIGHT, SCREEN_WIDTH, ScreenFrame};

    const RGB888_STRIDE: usize = SCREEN_WIDTH * 3;
    const RGB888_BYTES: usize = RGB888_STRIDE * SCREEN_HEIGHT;
    const OCR_SCALE: usize = 4;
    const OCR_WIDTH: usize = SCREEN_WIDTH * OCR_SCALE;
    const OCR_HEIGHT: usize = SCREEN_HEIGHT * OCR_SCALE;
    const OCR_STRIDE: usize = OCR_WIDTH * 3;
    const OCR_BYTES: usize = OCR_STRIDE * OCR_HEIGHT;
    const ERROR_BUFFER_CAPACITY: usize = 2_048;
    const MAX_OBSERVATIONS: usize = 4_096;
    const MAX_TEXT_BYTES: usize = 16_384;

    type ObservationCallback = unsafe extern "C" fn(
        context: *mut c_void,
        utf8: *const u8,
        utf8_len: usize,
        confidence: f32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    );

    unsafe extern "C" {
        fn thd75_vision_recognize_rgb888(
            rgb: *const u8,
            rgb_len: usize,
            width: usize,
            height: usize,
            bytes_per_row: usize,
            callback: ObservationCallback,
            context: *mut c_void,
            error_buffer: *mut u8,
            error_capacity: usize,
        ) -> i32;
    }

    #[derive(Debug, Default)]
    struct CallbackState {
        observations: Vec<TextObservation>,
        error: Option<VisionOcrError>,
    }

    pub(super) fn recognize_screen(
        frame: &ScreenFrame,
    ) -> Result<Vec<TextObservation>, VisionOcrError> {
        let rgb = frame.to_rgb888();
        recognize_rgb888(&rgb)
    }

    fn recognize_rgb888(rgb: &[u8]) -> Result<Vec<TextObservation>, VisionOcrError> {
        if rgb.len() != RGB888_BYTES {
            return Err(VisionOcrError::InvalidRgbLength {
                actual: rgb.len(),
                expected: RGB888_BYTES,
            });
        }

        let enlarged = enlarge_nearest_neighbor(rgb);
        if enlarged.len() != OCR_BYTES {
            return Err(VisionOcrError::InvalidRgbLength {
                actual: enlarged.len(),
                expected: OCR_BYTES,
            });
        }

        let mut observations =
            recognize_one_scale(rgb, SCREEN_WIDTH, SCREEN_HEIGHT, RGB888_STRIDE)?;
        let enlarged_observations =
            recognize_one_scale(&enlarged, OCR_WIDTH, OCR_HEIGHT, OCR_STRIDE)?;
        merge_observations(&mut observations, enlarged_observations);
        observations.sort_by(|left, right| {
            left.bounds()
                .y()
                .total_cmp(&right.bounds().y())
                .then_with(|| left.bounds().x().total_cmp(&right.bounds().x()))
                .then_with(|| left.text().cmp(right.text()))
                .then_with(|| right.confidence().total_cmp(&left.confidence()))
        });
        Ok(observations)
    }

    fn recognize_one_scale(
        rgb: &[u8],
        width: usize,
        height: usize,
        stride: usize,
    ) -> Result<Vec<TextObservation>, VisionOcrError> {
        let mut state = CallbackState::default();
        let mut error_buffer = [0_u8; ERROR_BUFFER_CAPACITY];
        // SAFETY: `rgb` has the caller-validated size for `width`, `height`, and
        // `stride` and remains alive for this synchronous call. `state` and the
        // error buffer are uniquely borrowed with stable addresses. The callback
        // validates every native pointer and length, and native code retains none.
        let status = unsafe {
            thd75_vision_recognize_rgb888(
                rgb.as_ptr(),
                rgb.len(),
                width,
                height,
                stride,
                collect_observation,
                (&raw mut state).cast(),
                error_buffer.as_mut_ptr(),
                error_buffer.len(),
            )
        };

        if let Some(error) = state.error {
            return Err(error);
        }
        if status != 0 {
            return Err(native_failure(status, &error_buffer));
        }
        Ok(state.observations)
    }

    fn merge_observations(
        observations: &mut Vec<TextObservation>,
        additional: Vec<TextObservation>,
    ) {
        for candidate in additional {
            let duplicate = observations.iter_mut().find(|existing| {
                existing.text() == candidate.text()
                    && overlap_fraction(existing.bounds(), candidate.bounds()) >= 0.5
            });
            if let Some(existing) = duplicate {
                if candidate.confidence() > existing.confidence() {
                    *existing = candidate;
                }
            } else {
                observations.push(candidate);
            }
        }
    }

    fn overlap_fraction(left: NormalizedBounds, right: NormalizedBounds) -> f32 {
        let intersection_width =
            (left.x() + left.width()).min(right.x() + right.width()) - left.x().max(right.x());
        let intersection_height =
            (left.y() + left.height()).min(right.y() + right.height()) - left.y().max(right.y());
        if intersection_width <= 0.0 || intersection_height <= 0.0 {
            return 0.0;
        }
        let intersection = intersection_width * intersection_height;
        let smaller_area = (left.width() * left.height()).min(right.width() * right.height());
        intersection / smaller_area
    }

    fn enlarge_nearest_neighbor(rgb: &[u8]) -> Vec<u8> {
        let mut enlarged = Vec::with_capacity(OCR_BYTES);
        let mut enlarged_row = Vec::with_capacity(OCR_STRIDE);
        for row in rgb.chunks_exact(RGB888_STRIDE) {
            enlarged_row.clear();
            for pixel in row.chunks_exact(3) {
                for _ in 0..OCR_SCALE {
                    enlarged_row.extend_from_slice(pixel);
                }
            }
            for _ in 0..OCR_SCALE {
                enlarged.extend_from_slice(&enlarged_row);
            }
        }
        enlarged
    }

    fn native_failure(status: i32, error_buffer: &[u8]) -> VisionOcrError {
        let message_end = error_buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(error_buffer.len());
        let message_bytes = error_buffer.get(..message_end).unwrap_or_default();
        let message = String::from_utf8_lossy(message_bytes).into_owned();
        let message = if message.is_empty() {
            "native bridge returned no diagnostic".to_owned()
        } else {
            message
        };
        VisionOcrError::NativeFailure { status, message }
    }

    unsafe extern "C" fn collect_observation(
        context: *mut c_void,
        utf8: *const u8,
        utf8_len: usize,
        confidence: f32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) {
        if context.is_null() {
            return;
        }
        // SAFETY: The only caller is the synchronous native bridge, which receives
        // this pointer from `recognize_rgb888`. It points to a live, uniquely
        // borrowed `CallbackState` for the full native call and is never retained.
        let state = unsafe { &mut *context.cast::<CallbackState>() };
        if state.error.is_some() {
            return;
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            decode_observation(utf8, utf8_len, confidence, x, y, width, height)
        }));
        match result {
            Ok(Ok(observation)) if state.observations.len() < MAX_OBSERVATIONS => {
                state.observations.push(observation);
            }
            Ok(Ok(_)) => {
                state.error = Some(VisionOcrError::TooManyObservations {
                    limit: MAX_OBSERVATIONS,
                });
            }
            Ok(Err(error)) => state.error = Some(error),
            Err(_) => state.error = Some(VisionOcrError::CallbackPanicked),
        }
    }

    fn decode_observation(
        utf8: *const u8,
        utf8_len: usize,
        confidence: f32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<TextObservation, VisionOcrError> {
        if utf8_len > MAX_TEXT_BYTES {
            return Err(VisionOcrError::TextTooLong {
                limit: MAX_TEXT_BYTES,
            });
        }
        if utf8.is_null() || utf8_len == 0 {
            return Err(VisionOcrError::InvalidObservation {
                reason: "native text pointer was null or empty".to_owned(),
            });
        }

        // SAFETY: The native callback contract makes `utf8` readable for exactly
        // `utf8_len` bytes during this callback. Null and zero length were rejected,
        // and the length is bounded before constructing the temporary slice.
        let bytes = unsafe { slice::from_raw_parts(utf8, utf8_len) };
        let text = str::from_utf8(bytes).map_err(|_| VisionOcrError::InvalidUtf8)?;
        let bounds = NormalizedBounds::new(x, y, width, height).map_err(|error| {
            VisionOcrError::InvalidObservation {
                reason: error.to_string(),
            }
        })?;
        TextObservation::new(text.to_owned(), confidence, bounds).map_err(|error| {
            VisionOcrError::InvalidObservation {
                reason: error.to_string(),
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::{NormalizedBounds, TextObservation, merge_observations};

        type TestResult = Result<(), Box<dyn std::error::Error>>;

        #[test]
        fn dual_scale_merge_deduplicates_overlap_but_preserves_real_repeats() -> TestResult {
            let mut observations = vec![TextObservation::new(
                "APRS",
                0.8,
                NormalizedBounds::new(0.40, 0.40, 0.20, 0.08)?,
            )?];
            let additional = vec![
                TextObservation::new("APRS", 0.9, NormalizedBounds::new(0.41, 0.41, 0.19, 0.07)?)?,
                TextObservation::new("APRS", 1.0, NormalizedBounds::new(0.39, 0.78, 0.21, 0.11)?)?,
                TextObservation::new("Menu", 1.0, NormalizedBounds::new(0.01, 0.02, 0.16, 0.10)?)?,
            ];

            merge_observations(&mut observations, additional);

            assert_eq!(observations.len(), 3);
            assert_eq!(
                observations
                    .iter()
                    .filter(|item| item.text() == "APRS")
                    .count(),
                2
            );
            assert_eq!(
                observations
                    .iter()
                    .find(|item| item.bounds().y() < 0.5)
                    .map(TextObservation::confidence),
                Some(0.9)
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundsError, NormalizedBounds, TextMatchError, TextObservation, require_unique_text,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn observation(
        text: &str,
        confidence: f32,
        bounds: NormalizedBounds,
    ) -> Result<TextObservation, Box<dyn std::error::Error>> {
        Ok(TextObservation::new(text, confidence, bounds)?)
    }

    #[test]
    fn bounds_reject_invalid_geometry() {
        assert_eq!(
            NormalizedBounds::new(f32::NAN, 0.0, 0.5, 0.5),
            Err(BoundsError::NonFinite)
        );
        assert_eq!(
            NormalizedBounds::new(-0.1, 0.0, 0.5, 0.5),
            Err(BoundsError::OriginOutsideScreen)
        );
        assert_eq!(
            NormalizedBounds::new(0.0, 0.0, 0.0, 0.5),
            Err(BoundsError::Empty)
        );
        assert_eq!(
            NormalizedBounds::new(0.8, 0.0, 0.3, 0.5),
            Err(BoundsError::ExtendsOutsideScreen)
        );
    }

    #[test]
    fn unique_exact_match_passes_confidence_and_roi() -> TestResult {
        let bounds = NormalizedBounds::new(0.2, 0.3, 0.2, 0.1)?;
        let observations = vec![
            observation("USB", 0.97, bounds)?,
            observation("GPS", 0.99, bounds)?,
        ];
        let roi = NormalizedBounds::new(0.15, 0.25, 0.4, 0.3)?;

        let matched = require_unique_text(&observations, "USB", 0.95, roi, 1.0)?;
        assert_eq!(matched.text(), "USB");
        Ok(())
    }

    #[test]
    fn matching_is_exact_and_case_sensitive() -> TestResult {
        let observations = vec![
            observation("USB ", 0.99, NormalizedBounds::FULL_SCREEN)?,
            observation("usb", 0.99, NormalizedBounds::FULL_SCREEN)?,
        ];

        assert_eq!(
            require_unique_text(
                &observations,
                "USB",
                0.0,
                NormalizedBounds::FULL_SCREEN,
                1.0,
            ),
            Err(TextMatchError::Missing {
                expected: "USB".to_owned(),
            })
        );
        Ok(())
    }

    #[test]
    fn low_confidence_or_insufficient_roi_coverage_is_missing() -> TestResult {
        let bounds = NormalizedBounds::new(0.4, 0.4, 0.2, 0.2)?;
        let observations = vec![observation("Storage", 0.89, bounds)?];
        let half_roi = NormalizedBounds::new(0.4, 0.4, 0.1, 0.2)?;

        assert!(matches!(
            require_unique_text(&observations, "Storage", 0.90, half_roi, 0.5),
            Err(TextMatchError::Missing { .. })
        ));
        assert!(matches!(
            require_unique_text(&observations, "Storage", 0.80, half_roi, 0.51),
            Err(TextMatchError::Missing { .. })
        ));
        Ok(())
    }

    #[test]
    fn duplicate_qualifying_text_is_ambiguous() -> TestResult {
        let bounds = NormalizedBounds::new(0.1, 0.1, 0.2, 0.1)?;
        let observations = vec![
            observation("On", 0.99, bounds)?,
            observation("On", 0.95, bounds)?,
            observation("On", 0.40, bounds)?,
        ];

        assert_eq!(
            require_unique_text(
                &observations,
                "On",
                0.90,
                NormalizedBounds::FULL_SCREEN,
                1.0,
            ),
            Err(TextMatchError::Ambiguous {
                expected: "On".to_owned(),
                matches: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn invalid_expected_text_and_thresholds_are_explicit() {
        assert_eq!(
            require_unique_text(&[], " ", 0.0, NormalizedBounds::FULL_SCREEN, 1.0,),
            Err(TextMatchError::BlankExpectedText)
        );
        assert!(matches!(
            require_unique_text(&[], "USB", f32::NAN, NormalizedBounds::FULL_SCREEN, 1.0,),
            Err(TextMatchError::InvalidThreshold {
                name: "minimum confidence",
                ..
            })
        ));
        assert_eq!(
            require_unique_text(&[], "USB", 0.0, NormalizedBounds::FULL_SCREEN, 1.1,),
            Err(TextMatchError::InvalidThreshold {
                name: "minimum ROI coverage",
                value: 1.1,
            })
        );
    }
}
