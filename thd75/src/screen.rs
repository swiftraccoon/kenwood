//! Canonical representation of the TH-D75's live LCD framebuffer.
//!
//! Stock V1.03 renders into a fixed 240 by 180, top-down, little-endian
//! RGB565 framebuffer.  This module keeps that native byte representation so
//! host-side validation can hash and compare exactly what the radio displayed,
//! then offers deterministic RGB and stock-compatible BMP conversions for
//! recognition and evidence artifacts.

use thiserror::Error;

pub mod ui;
#[cfg(any(target_os = "macos", doc))]
pub mod vision;

/// LCD width in pixels.
pub const SCREEN_WIDTH: usize = 240;
/// LCD height in pixels.
pub const SCREEN_HEIGHT: usize = 180;
/// Native framebuffer bytes per row.
pub const SCREEN_STRIDE: usize = SCREEN_WIDTH * 2;
/// Complete native framebuffer length.
pub const SCREEN_BYTES: usize = SCREEN_STRIDE * SCREEN_HEIGHT;

const STOCK_BMP_HEADER: [u8; 54] = [
    0x42, 0x4D, 0x76, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28, 0x00,
    0x00, 0x00, 0xF0, 0x00, 0x00, 0x00, 0xB4, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x40, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const fn zero_extend_rgb565(pixel: u16) -> [u8; 3] {
    let [low, high] = pixel.to_le_bytes();
    let red = high & 0xF8;
    let green = ((high & 0x07) << 5) | ((low & 0xE0) >> 3);
    let blue = (low & 0x1F) << 3;
    [red, green, blue]
}

/// A malformed native LCD frame or out-of-range pixel coordinate.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScreenError {
    /// Native data did not contain exactly one complete framebuffer.
    #[error("RGB565 framebuffer is {actual} bytes, expected exactly {expected}")]
    InvalidLength {
        /// Observed byte count.
        actual: usize,
        /// Required byte count.
        expected: usize,
    },
    /// A pixel coordinate lies outside the fixed LCD geometry.
    #[error("screen coordinate ({x},{y}) is outside {width}x{height} LCD geometry")]
    CoordinateOutOfRange {
        /// Horizontal coordinate.
        x: usize,
        /// Vertical coordinate.
        y: usize,
        /// Fixed screen width.
        width: usize,
        /// Fixed screen height.
        height: usize,
    },
}

/// One exact, top-down RGB565LE TH-D75 LCD frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFrame {
    rgb565_le: Box<[u8; SCREEN_BYTES]>,
}

impl ScreenFrame {
    /// Validate and construct a frame from native LCD bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenError::InvalidLength`] unless `bytes` contains exactly
    /// [`SCREEN_BYTES`] bytes.
    pub fn from_rgb565_le(bytes: Vec<u8>) -> Result<Self, ScreenError> {
        let actual = bytes.len();
        let rgb565_le =
            bytes
                .into_boxed_slice()
                .try_into()
                .map_err(|_bytes| ScreenError::InvalidLength {
                    actual,
                    expected: SCREEN_BYTES,
                })?;
        Ok(Self { rgb565_le })
    }

    /// Native top-down RGB565 little-endian bytes, exactly as published by the
    /// radio.
    #[must_use]
    pub fn rgb565_le(&self) -> &[u8] {
        self.rgb565_le.as_slice()
    }

    /// Return one native RGB565 pixel.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenError::CoordinateOutOfRange`] when `x` or `y` lies
    /// outside the fixed LCD geometry.
    pub fn pixel(&self, x: usize, y: usize) -> Result<u16, ScreenError> {
        if x >= SCREEN_WIDTH || y >= SCREEN_HEIGHT {
            return Err(ScreenError::CoordinateOutOfRange {
                x,
                y,
                width: SCREEN_WIDTH,
                height: SCREEN_HEIGHT,
            });
        }
        let offset = y * SCREEN_STRIDE + x * 2;
        let pair = self
            .rgb565_le
            .get(offset..offset + 2)
            .and_then(|bytes| <&[u8; 2]>::try_from(bytes).ok())
            .ok_or(ScreenError::CoordinateOutOfRange {
                x,
                y,
                width: SCREEN_WIDTH,
                height: SCREEN_HEIGHT,
            })?;
        Ok(u16::from_le_bytes(*pair))
    }

    /// Convert to top-down, tightly packed RGB888.
    ///
    /// Expansion matches the stock V1.03 screenshot routine: unused low bits
    /// are zero rather than replicated.
    #[must_use]
    pub fn to_rgb888(&self) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 3);
        let (pixels, _remainder) = self.rgb565_le.as_slice().as_chunks::<2>();
        for pair in pixels {
            rgb.extend_from_slice(&zero_extend_rgb565(u16::from_le_bytes(*pair)));
        }
        rgb
    }

    /// Render display-ready RGBA8888 bytes: [`Self::to_rgb888`] with an
    /// opaque alpha byte after every pixel, row-major top-down.
    ///
    /// This is the layout GPU texture uploads and most native image views
    /// consume directly, so FFI consumers need not append alpha per pixel.
    #[must_use]
    pub fn to_rgba8888(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 4);
        let (pixels, _remainder) = self.rgb565_le.as_slice().as_chunks::<2>();
        for pair in pixels {
            rgba.extend_from_slice(&zero_extend_rgb565(u16::from_le_bytes(*pair)));
            rgba.push(0xFF);
        }
        rgba
    }

    /// Render the same 24-bit, bottom-up BMP representation stock firmware
    /// writes for its Screen Capture function.
    #[must_use]
    pub fn to_stock_bmp(&self) -> Vec<u8> {
        let mut bmp = Vec::with_capacity(STOCK_BMP_HEADER.len() + SCREEN_WIDTH * SCREEN_HEIGHT * 3);
        bmp.extend_from_slice(&STOCK_BMP_HEADER);
        for row in self.rgb565_le.chunks_exact(SCREEN_STRIDE).rev() {
            let (pixels, _remainder) = row.as_chunks::<2>();
            for pair in pixels {
                let [red, green, blue] = zero_extend_rgb565(u16::from_le_bytes(*pair));
                bmp.extend_from_slice(&[blue, green, red]);
            }
        }
        bmp
    }

    /// Compute the standard reflected IEEE CRC-32 used by the automation
    /// firmware metadata.
    #[must_use]
    pub fn crc32(&self) -> u32 {
        let mut crc = u32::MAX;
        for byte in self.rgb565_le.as_slice() {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let low_bit_mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xEDB8_8320 & low_bit_mask);
            }
        }
        !crc
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SCREEN_BYTES, SCREEN_HEIGHT, SCREEN_STRIDE, SCREEN_WIDTH, STOCK_BMP_HEADER, ScreenError,
        ScreenFrame, zero_extend_rgb565,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn rejects_noncanonical_length() {
        let error = ScreenFrame::from_rgb565_le(vec![0; SCREEN_BYTES - 1]);
        assert_eq!(
            error,
            Err(ScreenError::InvalidLength {
                actual: SCREEN_BYTES - 1,
                expected: SCREEN_BYTES,
            })
        );
    }

    #[test]
    fn reads_top_down_little_endian_pixels() -> TestResult {
        let mut bytes = vec![0; SCREEN_BYTES];
        let offset = 7 * SCREEN_STRIDE + 11 * 2;
        let destination = bytes
            .get_mut(offset..offset + 2)
            .ok_or("test pixel destination is out of range")?;
        destination.copy_from_slice(&0xF81F_u16.to_le_bytes());
        let frame = ScreenFrame::from_rgb565_le(bytes)?;
        assert_eq!(frame.pixel(11, 7)?, 0xF81F);
        assert!(matches!(
            frame.pixel(SCREEN_WIDTH, 0),
            Err(ScreenError::CoordinateOutOfRange { .. })
        ));
        assert!(matches!(
            frame.pixel(0, SCREEN_HEIGHT),
            Err(ScreenError::CoordinateOutOfRange { .. })
        ));
        Ok(())
    }

    #[test]
    fn rgb565_zero_extension_preserves_every_component_boundary() {
        assert_eq!(zero_extend_rgb565(0x0000), [0x00, 0x00, 0x00]);
        assert_eq!(zero_extend_rgb565(0xF800), [0xF8, 0x00, 0x00]);
        assert_eq!(zero_extend_rgb565(0x07E0), [0x00, 0xFC, 0x00]);
        assert_eq!(zero_extend_rgb565(0x001F), [0x00, 0x00, 0xF8]);
        assert_eq!(zero_extend_rgb565(0xFFFF), [0xF8, 0xFC, 0xF8]);
    }

    #[test]
    fn rgb_and_bmp_match_stock_zero_extended_conversion() -> TestResult {
        let mut bytes = vec![0; SCREEN_BYTES];
        bytes
            .get_mut(..2)
            .ok_or("test frame is missing its first pixel")?
            .copy_from_slice(&0xFFFF_u16.to_le_bytes());
        let bottom_left = (SCREEN_HEIGHT - 1) * SCREEN_STRIDE;
        bytes
            .get_mut(bottom_left..bottom_left + 2)
            .ok_or("test frame is missing its bottom-left pixel")?
            .copy_from_slice(&0xF800_u16.to_le_bytes());
        let frame = ScreenFrame::from_rgb565_le(bytes)?;

        assert_eq!(
            frame.to_rgb888().get(..3),
            Some([0xF8, 0xFC, 0xF8].as_slice())
        );
        let bmp = frame.to_stock_bmp();
        assert_eq!(bmp.get(..54), Some(STOCK_BMP_HEADER.as_slice()));
        assert_eq!(bmp.len(), 129_654);
        assert_eq!(bmp.get(54..57), Some([0, 0, 0xF8].as_slice()));
        Ok(())
    }

    #[test]
    fn crc32_matches_standard_check_vector() -> TestResult {
        let mut bytes = vec![0; SCREEN_BYTES];
        bytes
            .get_mut(..9)
            .ok_or("test frame cannot hold CRC check vector")?
            .copy_from_slice(b"123456789");
        let frame = ScreenFrame::from_rgb565_le(bytes)?;

        assert_eq!(frame.crc32(), 0x9877_0B66);
        Ok(())
    }

    #[test]
    fn rgba8888_is_rgb888_with_opaque_alpha() -> TestResult {
        let mut bytes = vec![0_u8; SCREEN_BYTES];
        // A recognizable first pixel: RGB565 pure red, little-endian.
        bytes
            .get_mut(..2)
            .ok_or("test frame cannot hold one pixel")?
            .copy_from_slice(&0xF800_u16.to_le_bytes());
        let frame = ScreenFrame::from_rgb565_le(bytes)?;

        let rgb = frame.to_rgb888();
        let rgba = frame.to_rgba8888();
        assert_eq!(rgba.len(), SCREEN_WIDTH * SCREEN_HEIGHT * 4);
        for (pixel_rgb, pixel_rgba) in rgb.chunks_exact(3).zip(rgba.chunks_exact(4)) {
            assert_eq!(
                pixel_rgba.get(..3),
                Some(pixel_rgb),
                "color bytes must match to_rgb888"
            );
            assert_eq!(pixel_rgba.get(3), Some(&0xFF), "alpha must be opaque");
        }
        Ok(())
    }
}
