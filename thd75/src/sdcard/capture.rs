//! Parser for BMP screen capture files.
//!
//! The TH-D75 saves screenshots as standard BMP bitmap files.
//! Per User Manual Chapter 19 and Operating Tips §5.14:
//!
//! - Format: 240x180 pixels, 24-bit RGB (uncompressed).
//! - Files are stored in `/KENWOOD/TH-D75/CAPTURE/*.bmp`.
//! - Maximum 255 files per directory.
//!
//! # Location
//!
//! `/KENWOOD/TH-D75/CAPTURE/*.bmp`
//!
//! # Details
//!
//! This parser validates the BMP and DIB headers, verifies the
//! dimensions and bit depth match the TH-D75 display, and extracts
//! the raw BGR pixel data. BMP files store rows bottom-up by default.

use super::{SdCardError, read_u16_le, read_u32_le};

/// Expected screen width in pixels.
const EXPECTED_WIDTH: u32 = 240;

/// Expected screen height in pixels.
const EXPECTED_HEIGHT: u32 = 180;

/// Expected bits per pixel.
const EXPECTED_BPP: u16 = 24;

/// BMP file header size (14 bytes).
const BMP_HEADER_SIZE: usize = 14;

/// Minimum DIB (BITMAPINFOHEADER) size (40 bytes).
const MIN_DIB_HEADER_SIZE: u32 = 40;

/// Minimum BMP file size: file header + DIB header.
const MIN_BMP_SIZE: usize = BMP_HEADER_SIZE + MIN_DIB_HEADER_SIZE as usize;

/// BMP compression type for uncompressed (`BI_RGB`).
const BI_RGB: u32 = 0;

/// A parsed TH-D75 screen capture.
///
/// Contains the validated image metadata and raw BGR pixel data
/// as stored in the BMP file (bottom-up row order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCapture {
    /// Image width in pixels. Expected: 240 for TH-D75.
    pub width: u32,
    /// Image height in pixels. Expected: 180 for TH-D75.
    pub height: u32,
    /// Bits per pixel. Expected: 24 for TH-D75.
    pub bits_per_pixel: u16,
    /// Raw BGR pixel data in bottom-up row order.
    ///
    /// Each pixel is 3 bytes: blue, green, red. Rows are stored
    /// from the bottom of the image to the top, as is standard
    /// for BMP files. Row padding (to 4-byte alignment) is stripped.
    pub pixels: Vec<u8>,
}

/// Read a little-endian `i32` from a byte slice at the given offset.
///
/// Returns `0` if the slice is too short — the caller is expected to have
/// validated the buffer length before calling.
fn read_i32_le(data: &[u8], offset: usize) -> i32 {
    data.get(offset..offset + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, i32::from_le_bytes)
}

/// Parse a BMP screen capture file from raw bytes.
///
/// Validates the BMP file header, DIB header, dimensions, and bit
/// depth. Extracts the raw BGR pixel data with row padding removed.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the data is shorter than
/// the minimum BMP header size (54 bytes).
///
/// Returns [`SdCardError::InvalidBmpHeader`] if the BM magic bytes,
/// DIB header size, or compression type is invalid.
///
/// Returns [`SdCardError::UnexpectedImageFormat`] if the width,
/// height, or bit depth does not match the expected TH-D75 screen
/// dimensions (240x180, 24-bit).
pub fn parse(data: &[u8]) -> Result<ScreenCapture, SdCardError> {
    if data.len() < MIN_BMP_SIZE {
        return Err(SdCardError::FileTooSmall {
            expected: MIN_BMP_SIZE,
            actual: data.len(),
        });
    }

    // Validate BM magic bytes.
    if data.get(..2) != Some(b"BM") {
        return Err(SdCardError::InvalidBmpHeader {
            detail: "missing BM magic bytes".to_owned(),
        });
    }

    // Pixel data offset from file header.
    let pixel_offset = read_u32_le(data, 10) as usize;

    // DIB header size (at offset 14).
    let dib_size = read_u32_le(data, 14);
    if dib_size < MIN_DIB_HEADER_SIZE {
        return Err(SdCardError::InvalidBmpHeader {
            detail: format!("DIB header size {dib_size} too small (minimum {MIN_DIB_HEADER_SIZE})"),
        });
    }

    // Image dimensions. Height can be negative (top-down), but TH-D75
    // uses standard bottom-up, so we read as signed and take absolute value.
    let raw_width = read_i32_le(data, 18);
    let raw_height = read_i32_le(data, 22);

    let Ok(width) = u32::try_from(raw_width) else {
        return Err(SdCardError::InvalidBmpHeader {
            detail: format!("invalid width {raw_width}"),
        });
    };
    if width == 0 {
        return Err(SdCardError::InvalidBmpHeader {
            detail: "width is zero".to_owned(),
        });
    }

    let height = raw_height.unsigned_abs();

    let bits_per_pixel = read_u16_le(data, 28);

    // Compression (offset 30).
    let compression = read_u32_le(data, 30);
    if compression != BI_RGB {
        return Err(SdCardError::InvalidBmpHeader {
            detail: format!("unsupported compression type {compression} (expected 0 for BI_RGB)"),
        });
    }

    // Validate TH-D75 expected format.
    if width != EXPECTED_WIDTH || height != EXPECTED_HEIGHT || bits_per_pixel != EXPECTED_BPP {
        return Err(SdCardError::UnexpectedImageFormat {
            width,
            height,
            bits_per_pixel,
        });
    }

    // Calculate row stride with padding to 4-byte boundary.
    let bytes_per_row = u32::from(bits_per_pixel) / 8 * width;
    let row_stride = (bytes_per_row + 3) & !3;

    let pixel_data_size = row_stride as usize * height as usize;
    let required_size = pixel_offset + pixel_data_size;

    if data.len() < required_size {
        return Err(SdCardError::FileTooSmall {
            expected: required_size,
            actual: data.len(),
        });
    }

    // Extract pixel data, stripping row padding if present.
    let pixels = if row_stride == bytes_per_row {
        data.get(pixel_offset..pixel_offset + pixel_data_size)
            .ok_or(SdCardError::FileTooSmall {
                expected: pixel_offset + pixel_data_size,
                actual: data.len(),
            })?
            .to_vec()
    } else {
        let mut pixels = Vec::with_capacity(bytes_per_row as usize * height as usize);
        for row in 0..height as usize {
            let row_start = pixel_offset + row * row_stride as usize;
            let row_end = row_start + bytes_per_row as usize;
            let row_slice = data
                .get(row_start..row_end)
                .ok_or(SdCardError::FileTooSmall {
                    expected: row_end,
                    actual: data.len(),
                })?;
            pixels.extend_from_slice(row_slice);
        }
        pixels
    };

    Ok(ScreenCapture {
        width,
        height,
        bits_per_pixel,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid BMP file with the given parameters.
    fn build_bmp(width: u32, height: u32, bpp: u16) -> Vec<u8> {
        let bytes_per_row = u32::from(bpp) / 8 * width;
        let row_stride = (bytes_per_row + 3) & !3;
        let pixel_data_size = row_stride * height;
        let file_size = 54 + pixel_data_size;

        let mut buf = Vec::with_capacity(file_size as usize);

        // BMP file header (14 bytes)
        buf.extend_from_slice(b"BM");
        buf.extend_from_slice(&file_size.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved1
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved2
        buf.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset

        // DIB header (BITMAPINFOHEADER, 40 bytes)
        buf.extend_from_slice(&40u32.to_le_bytes()); // header size
        #[expect(
            clippy::cast_possible_wrap,
            reason = "Test helper builds synthetic BMP for parser round-trip. BMP DIB width is a \
                      signed i32 per Microsoft's BITMAPINFOHEADER spec; test inputs are small \
                      positive values (<= 2^31-1), so the cast from u32 never wraps."
        )]
        buf.extend_from_slice(&(width as i32).to_le_bytes());
        #[expect(
            clippy::cast_possible_wrap,
            reason = "BMP DIB height is i32 (negative indicates top-down rows) per Microsoft's \
                      BITMAPINFOHEADER spec. Test-only values stay well below i32::MAX."
        )]
        buf.extend_from_slice(&(height as i32).to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // planes
        buf.extend_from_slice(&bpp.to_le_bytes());
        buf.extend_from_slice(&BI_RGB.to_le_bytes()); // compression
        buf.extend_from_slice(&pixel_data_size.to_le_bytes()); // image size
        buf.extend_from_slice(&2835u32.to_le_bytes()); // x pixels per meter
        buf.extend_from_slice(&2835u32.to_le_bytes()); // y pixels per meter
        buf.extend_from_slice(&0u32.to_le_bytes()); // colors used
        buf.extend_from_slice(&0u32.to_le_bytes()); // important colors

        // Pixel data (fill with a recognisable pattern).
        for row in 0..height {
            for col in 0..width {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "Synthesises a recognisable test pixel pattern. `x % 256` is \
                              provably in 0..=255, so the u32-to-u8 cast cannot truncate."
                )]
                let b = ((row + col) % 256) as u8;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "Synthesises a recognisable test pixel pattern. `x % 256` is \
                              provably in 0..=255, so the u32-to-u8 cast cannot truncate."
                )]
                let g = ((row * 2 + col) % 256) as u8;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "Synthesises a recognisable test pixel pattern. `x % 256` is \
                              provably in 0..=255, so the u32-to-u8 cast cannot truncate."
                )]
                let r = ((row + col * 2) % 256) as u8;
                buf.push(b);
                buf.push(g);
                buf.push(r);
            }
            // Padding bytes to reach row_stride.
            let padding = row_stride - bytes_per_row;
            buf.extend(std::iter::repeat_n(0u8, padding as usize));
        }

        buf
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn write_slice(image: &mut [u8], offset: usize, data: &[u8]) -> Result<(), BoxErr> {
        let end = offset + data.len();
        let img_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("write_slice: range {offset}..{end} out of bounds (len={img_len})")
            })?
            .copy_from_slice(data);
        Ok(())
    }

    /// Return `bytes[idx]` or an error if out of range.
    fn byte_at(bytes: &[u8], idx: usize) -> Result<u8, BoxErr> {
        bytes
            .get(idx)
            .copied()
            .ok_or_else(|| format!("byte_at: idx {idx} out of range (len={})", bytes.len()).into())
    }

    #[test]
    fn parse_valid_d75_capture() -> TestResult {
        let bmp = build_bmp(240, 180, 24);
        let cap = parse(&bmp)?;

        assert_eq!(cap.width, 240);
        assert_eq!(cap.height, 180);
        assert_eq!(cap.bits_per_pixel, 24);
        // 240 * 180 * 3 = 129600 bytes of pixel data (no padding needed: 240*3=720, divisible by 4)
        assert_eq!(cap.pixels.len(), 240 * 180 * 3);
        Ok(())
    }

    #[test]
    fn pixel_data_correct() -> TestResult {
        let bmp = build_bmp(240, 180, 24);
        let cap = parse(&bmp)?;

        // Verify first pixel (row 0, col 0): b=0, g=0, r=0
        assert_eq!(byte_at(&cap.pixels, 0)?, 0); // blue
        assert_eq!(byte_at(&cap.pixels, 1)?, 0); // green
        assert_eq!(byte_at(&cap.pixels, 2)?, 0); // red

        // Verify second pixel (row 0, col 1): b=1, g=1, r=2
        assert_eq!(byte_at(&cap.pixels, 3)?, 1);
        assert_eq!(byte_at(&cap.pixels, 4)?, 1);
        assert_eq!(byte_at(&cap.pixels, 5)?, 2);
        Ok(())
    }

    #[test]
    fn too_short_returns_error() -> TestResult {
        let data = b"BM\x00\x00";
        let err = parse(data)
            .err()
            .ok_or("expected FileTooSmall but got Ok")?;
        assert!(
            matches!(err, SdCardError::FileTooSmall { .. }),
            "expected FileTooSmall, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn empty_returns_error() -> TestResult {
        let err = parse(b"").err().ok_or("expected FileTooSmall but got Ok")?;
        assert!(
            matches!(err, SdCardError::FileTooSmall { .. }),
            "expected FileTooSmall, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_magic_bytes() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 0, b"XX")?;
        let err = parse(&bmp)
            .err()
            .ok_or("expected InvalidBmpHeader but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidBmpHeader { .. }),
            "expected InvalidBmpHeader, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_dimensions_rejected() -> TestResult {
        let bmp = build_bmp(320, 240, 24);
        let err = parse(&bmp)
            .err()
            .ok_or("expected UnexpectedImageFormat but got Ok")?;
        assert!(
            matches!(err, SdCardError::UnexpectedImageFormat { .. }),
            "expected UnexpectedImageFormat, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn wrong_bit_depth_rejected() -> TestResult {
        let bmp = build_bmp(240, 180, 32);
        let err = parse(&bmp)
            .err()
            .ok_or("expected UnexpectedImageFormat but got Ok")?;
        assert!(
            matches!(err, SdCardError::UnexpectedImageFormat { .. }),
            "expected UnexpectedImageFormat, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn compressed_bmp_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        // Set compression to 1 (BI_RLE8) at offset 30.
        write_slice(&mut bmp, 30, &1u32.to_le_bytes())?;
        let err = parse(&bmp)
            .err()
            .ok_or("expected InvalidBmpHeader but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidBmpHeader { .. }),
            "expected InvalidBmpHeader, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn truncated_pixel_data_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        // Truncate to just the header.
        bmp.truncate(60);
        let err = parse(&bmp)
            .err()
            .ok_or("expected FileTooSmall but got Ok")?;
        assert!(
            matches!(err, SdCardError::FileTooSmall { .. }),
            "expected FileTooSmall, got {err:?}"
        );
        Ok(())
    }
}
