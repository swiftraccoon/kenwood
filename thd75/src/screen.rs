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

fn decode_rgb565_le(pair: &[u8]) -> Option<u16> {
    <[u8; 2]>::try_from(pair).ok().map(u16::from_le_bytes)
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
    rgb565_le: Box<[u8]>,
}

impl ScreenFrame {
    /// Validate and construct a frame from native LCD bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ScreenError::InvalidLength`] unless `bytes` contains exactly
    /// [`SCREEN_BYTES`] bytes.
    pub fn from_rgb565_le(bytes: Vec<u8>) -> Result<Self, ScreenError> {
        if bytes.len() != SCREEN_BYTES {
            return Err(ScreenError::InvalidLength {
                actual: bytes.len(),
                expected: SCREEN_BYTES,
            });
        }
        Ok(Self {
            rgb565_le: bytes.into_boxed_slice(),
        })
    }

    /// Native top-down RGB565 little-endian bytes, exactly as published by the
    /// radio.
    #[must_use]
    pub fn rgb565_le(&self) -> &[u8] {
        &self.rgb565_le
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
        self.rgb565_le
            .get(offset..offset + 2)
            .and_then(decode_rgb565_le)
            .ok_or(ScreenError::CoordinateOutOfRange {
                x,
                y,
                width: SCREEN_WIDTH,
                height: SCREEN_HEIGHT,
            })
    }

    /// Convert to top-down, tightly packed RGB888.
    ///
    /// Expansion matches the stock V1.03 screenshot routine: unused low bits
    /// are zero rather than replicated.
    #[must_use]
    pub fn to_rgb888(&self) -> Vec<u8> {
        let mut rgb = Vec::with_capacity(SCREEN_WIDTH * SCREEN_HEIGHT * 3);
        for pair in self.rgb565_le.chunks_exact(2) {
            if let Some(pixel) = decode_rgb565_le(pair) {
                let red = u8::try_from((pixel >> 8) & 0xF8).unwrap_or(0);
                let green = u8::try_from((pixel >> 3) & 0xFC).unwrap_or(0);
                let blue = u8::try_from((pixel << 3) & 0xF8).unwrap_or(0);
                rgb.extend_from_slice(&[red, green, blue]);
            }
        }
        rgb
    }

    /// Render the same 24-bit, bottom-up BMP representation stock firmware
    /// writes for its Screen Capture function.
    #[must_use]
    pub fn to_stock_bmp(&self) -> Vec<u8> {
        let mut bmp = Vec::with_capacity(STOCK_BMP_HEADER.len() + SCREEN_WIDTH * SCREEN_HEIGHT * 3);
        bmp.extend_from_slice(&STOCK_BMP_HEADER);
        for row in self.rgb565_le.chunks_exact(SCREEN_STRIDE).rev() {
            for pair in row.chunks_exact(2) {
                if let Some(pixel) = decode_rgb565_le(pair) {
                    let blue = u8::try_from((pixel << 3) & 0xF8).unwrap_or(0);
                    let green = u8::try_from((pixel >> 3) & 0xFC).unwrap_or(0);
                    let red = u8::try_from((pixel >> 8) & 0xF8).unwrap_or(0);
                    bmp.extend_from_slice(&[blue, green, red]);
                }
            }
        }
        bmp
    }

    /// Compute the standard reflected IEEE CRC-32 used by the automation
    /// firmware metadata.
    #[must_use]
    pub fn crc32(&self) -> u32 {
        let mut crc = u32::MAX;
        for byte in &self.rgb565_le {
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
        ScreenFrame,
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
}
