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
//! dimensions and bit depth match the TH-D75 display, and canonicalizes
//! the pixels to top-down RGB order. Native captures use a positive
//! height (bottom-up rows); valid negative-height BMPs decode identically.

use super::{SdCardError, read_u16_le, read_u32_le};

/// TH-D75 screen width in pixels.
pub const SCREEN_CAPTURE_WIDTH: u32 = 240;

/// TH-D75 screen height in pixels.
pub const SCREEN_CAPTURE_HEIGHT: u32 = 180;

/// Bit depth of a TH-D75 screen capture.
pub const SCREEN_CAPTURE_BITS_PER_PIXEL: u16 = 24;

/// Expected number of color planes.
const EXPECTED_PLANES: u16 = 1;

/// Bytes per canonical RGB pixel.
const RGB_BYTES_PER_PIXEL: usize = 3;

/// Exact number of bytes in the canonical top-down RGB888 pixel buffer.
pub const SCREEN_CAPTURE_RGB_BYTE_LEN: usize =
    SCREEN_CAPTURE_WIDTH as usize * SCREEN_CAPTURE_HEIGHT as usize * RGB_BYTES_PER_PIXEL;

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
/// The dimensions, bit depth, and pixel-buffer length are invariants of this
/// type. A value can only be obtained by parsing a structurally valid TH-D75
/// BMP capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCapture {
    pixels: Box<[u8; SCREEN_CAPTURE_RGB_BYTE_LEN]>,
}

impl ScreenCapture {
    /// Return the fixed TH-D75 screen width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        SCREEN_CAPTURE_WIDTH
    }

    /// Return the fixed TH-D75 screen height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        SCREEN_CAPTURE_HEIGHT
    }

    /// Return the fixed TH-D75 screen bit depth.
    #[must_use]
    pub const fn bits_per_pixel(&self) -> u16 {
        SCREEN_CAPTURE_BITS_PER_PIXEL
    }

    /// Borrow the exact-size RGB888 pixel buffer in top-down row order.
    ///
    /// Each pixel is three bytes in red, green, blue order. Row zero is the
    /// top display row. BMP row padding is not present.
    #[must_use]
    pub fn rgb888(&self) -> &[u8; SCREEN_CAPTURE_RGB_BYTE_LEN] {
        &self.pixels
    }

    /// Consume the capture and return its exact-size RGB888 pixel buffer.
    #[must_use]
    pub fn into_rgb888(self) -> Box<[u8; SCREEN_CAPTURE_RGB_BYTE_LEN]> {
        self.pixels
    }
}

/// Read a little-endian `i32` from a byte slice at the given offset.
///
/// Returns [`SdCardError::FileTooSmall`] if the field is truncated.
fn read_i32_le(data: &[u8], offset: usize) -> Result<i32, SdCardError> {
    Ok(i32::from_le_bytes(
        data.get(offset..offset.saturating_add(4))
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .ok_or_else(|| SdCardError::FileTooSmall {
                expected: offset.saturating_add(4),
                actual: data.len(),
            })?,
    ))
}

/// Parse a BMP screen capture file from raw bytes.
///
/// Validates the BMP file header, DIB header, declared sizes, pixel offset,
/// dimensions, planes, bit depth, and compression. Pixels are returned as
/// tightly packed, top-down RGB regardless of the source BMP's signed height.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the data is shorter than
/// the minimum BMP header size (54 bytes).
///
/// Returns [`SdCardError::InvalidBmpHeader`] if any structural field is
/// inconsistent, including declared sizes or a pixel offset inside the headers.
///
/// Returns [`SdCardError::UnexpectedImageFormat`] if the width,
/// height, or bit depth does not match the expected TH-D75 screen
/// dimensions (240x180, 24-bit).
pub fn parse(data: &[u8]) -> Result<ScreenCapture, SdCardError> {
    let file_header = parse_file_header(data)?;
    let dib_header = parse_dib_header(data)?;
    let layout = validate_pixel_layout(data, file_header, dib_header)?;
    let pixels = decode_pixels(data, file_header, dib_header, layout)?
        .into_boxed_slice()
        .try_into()
        .map_err(|pixels: Box<[u8]>| {
            invalid_bmp(format!(
                "decoded RGB buffer has {} bytes (expected {SCREEN_CAPTURE_RGB_BYTE_LEN})",
                pixels.len()
            ))
        })?;

    Ok(ScreenCapture { pixels })
}

#[derive(Debug, Clone, Copy)]
struct FileHeader {
    declared_file_size: usize,
    pixel_offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct DibHeader {
    width: u32,
    height: u32,
    raw_height: i32,
}

#[derive(Debug, Clone, Copy)]
struct PixelLayout {
    height: usize,
    bytes_per_row: usize,
    row_stride: usize,
    canonical_size: usize,
}

fn invalid_bmp(detail: impl Into<String>) -> SdCardError {
    SdCardError::InvalidBmpHeader {
        detail: detail.into(),
    }
}

fn usize_header_field(value: u32, field: &str) -> Result<usize, SdCardError> {
    usize::try_from(value).map_err(|_| invalid_bmp(format!("{field} does not fit this platform")))
}

fn parse_file_header(data: &[u8]) -> Result<FileHeader, SdCardError> {
    if data.len() < MIN_BMP_SIZE {
        return Err(SdCardError::FileTooSmall {
            expected: MIN_BMP_SIZE,
            actual: data.len(),
        });
    }

    // Validate BM magic bytes.
    if data.get(..2) != Some(b"BM") {
        return Err(invalid_bmp("missing BM magic bytes"));
    }

    let declared_file_size = usize_header_field(read_u32_le(data, 2)?, "declared file size")?;
    if declared_file_size != data.len() {
        return Err(invalid_bmp(format!(
            "declared file size {declared_file_size} does not match actual size {}",
            data.len()
        )));
    }
    let reserved_1 = read_u16_le(data, 6)?;
    let reserved_2 = read_u16_le(data, 8)?;
    if reserved_1 != 0 || reserved_2 != 0 {
        return Err(invalid_bmp(format!(
            "reserved file-header fields must be zero (got {reserved_1}, {reserved_2})"
        )));
    }

    let pixel_offset = usize_header_field(read_u32_le(data, 10)?, "pixel offset")?;

    let dib_size = read_u32_le(data, 14)?;
    if dib_size < MIN_DIB_HEADER_SIZE {
        return Err(invalid_bmp(format!(
            "DIB header size {dib_size} too small (minimum {MIN_DIB_HEADER_SIZE})"
        )));
    }
    let dib_size = usize_header_field(dib_size, "DIB header size")?;
    let dib_end = BMP_HEADER_SIZE
        .checked_add(dib_size)
        .ok_or_else(|| invalid_bmp("DIB header end overflows address space"))?;
    if data.len() < dib_end {
        return Err(SdCardError::FileTooSmall {
            expected: dib_end,
            actual: data.len(),
        });
    }
    if pixel_offset < dib_end {
        return Err(invalid_bmp(format!(
            "pixel offset {pixel_offset} points inside headers ending at {dib_end}"
        )));
    }

    Ok(FileHeader {
        declared_file_size,
        pixel_offset,
    })
}

fn parse_dib_header(data: &[u8]) -> Result<DibHeader, SdCardError> {
    // A positive height stores BMP rows bottom-up; a negative height stores
    // them top-down. The returned pixels are top-down in both cases.
    let raw_width = read_i32_le(data, 18)?;
    let raw_height = read_i32_le(data, 22)?;

    let Ok(width) = u32::try_from(raw_width) else {
        return Err(invalid_bmp(format!("invalid width {raw_width}")));
    };
    if width == 0 {
        return Err(invalid_bmp("width is zero"));
    }

    if raw_height == 0 {
        return Err(invalid_bmp("height is zero"));
    }
    if raw_height == i32::MIN {
        return Err(invalid_bmp(
            "height cannot be represented as an absolute value",
        ));
    }
    let height = raw_height.unsigned_abs();

    let planes = read_u16_le(data, 26)?;
    if planes != EXPECTED_PLANES {
        return Err(invalid_bmp(format!(
            "invalid color plane count {planes} (expected {EXPECTED_PLANES})"
        )));
    }

    let bits_per_pixel = read_u16_le(data, 28)?;
    let compression = read_u32_le(data, 30)?;
    if compression != BI_RGB {
        return Err(invalid_bmp(format!(
            "unsupported compression type {compression} (expected 0 for BI_RGB)"
        )));
    }

    if width != SCREEN_CAPTURE_WIDTH
        || height != SCREEN_CAPTURE_HEIGHT
        || bits_per_pixel != SCREEN_CAPTURE_BITS_PER_PIXEL
    {
        return Err(SdCardError::UnexpectedImageFormat {
            width,
            height,
            bits_per_pixel,
        });
    }

    Ok(DibHeader {
        width,
        height,
        raw_height,
    })
}

fn validate_pixel_layout(
    data: &[u8],
    file_header: FileHeader,
    dib_header: DibHeader,
) -> Result<PixelLayout, SdCardError> {
    let width = usize_header_field(dib_header.width, "width")?;
    let height = usize_header_field(dib_header.height, "height")?;

    // Calculate the BMP row stride, including padding to a four-byte boundary.
    let bytes_per_row = width
        .checked_mul(RGB_BYTES_PER_PIXEL)
        .ok_or_else(|| invalid_bmp("unpacked row size overflows address space"))?;
    let row_stride = bytes_per_row
        .checked_add(3)
        .map(|size| size & !3)
        .ok_or_else(|| invalid_bmp("padded row size overflows address space"))?;
    let pixel_data_size = row_stride
        .checked_mul(height)
        .ok_or_else(|| invalid_bmp("pixel data size overflows address space"))?;

    let declared_image_size = usize_header_field(read_u32_le(data, 34)?, "declared image size")?;
    if declared_image_size != pixel_data_size {
        return Err(invalid_bmp(format!(
            "declared image size {declared_image_size} does not match computed size \
             {pixel_data_size}"
        )));
    }

    let pixel_end = file_header
        .pixel_offset
        .checked_add(pixel_data_size)
        .ok_or_else(|| invalid_bmp("pixel data end overflows address space"))?;
    if data.len() < pixel_end {
        return Err(SdCardError::FileTooSmall {
            expected: pixel_end,
            actual: data.len(),
        });
    }
    if pixel_end != file_header.declared_file_size {
        return Err(invalid_bmp(format!(
            "pixel data ends at {pixel_end}, but declared file size is {}",
            file_header.declared_file_size
        )));
    }

    let canonical_size = bytes_per_row
        .checked_mul(height)
        .ok_or_else(|| invalid_bmp("canonical pixel data size overflows address space"))?;

    Ok(PixelLayout {
        height,
        bytes_per_row,
        row_stride,
        canonical_size,
    })
}

fn decode_pixels(
    data: &[u8],
    file_header: FileHeader,
    dib_header: DibHeader,
    layout: PixelLayout,
) -> Result<Vec<u8>, SdCardError> {
    let mut pixels = Vec::with_capacity(layout.canonical_size);
    for output_row in 0..layout.height {
        let source_row = if dib_header.raw_height.is_positive() {
            layout.height - 1 - output_row
        } else {
            output_row
        };
        let row_offset = source_row
            .checked_mul(layout.row_stride)
            .ok_or_else(|| invalid_bmp("source row offset overflows address space"))?;
        let row_start = file_header
            .pixel_offset
            .checked_add(row_offset)
            .ok_or_else(|| invalid_bmp("source row start overflows address space"))?;
        let row_end = row_start
            .checked_add(layout.bytes_per_row)
            .ok_or_else(|| invalid_bmp("source row end overflows address space"))?;
        let row = data
            .get(row_start..row_end)
            .ok_or(SdCardError::FileTooSmall {
                expected: row_end,
                actual: data.len(),
            })?;
        for bgr in row.chunks_exact(RGB_BYTES_PER_PIXEL) {
            let &[blue, green, red] = bgr else {
                return Err(invalid_bmp(
                    "pixel row is not a whole number of 24-bit pixels",
                ));
            };
            pixels.extend_from_slice(&[red, green, blue]);
        }
    }

    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a structurally complete BMP whose logical pixels are independent
    /// of the source row orientation.
    fn build_bmp(width: u32, signed_height: i32, bpp: u16) -> Vec<u8> {
        let height = signed_height.unsigned_abs();
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
        buf.extend_from_slice(&signed_height.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // planes
        buf.extend_from_slice(&bpp.to_le_bytes());
        buf.extend_from_slice(&BI_RGB.to_le_bytes()); // compression
        buf.extend_from_slice(&pixel_data_size.to_le_bytes()); // image size
        buf.extend_from_slice(&2835u32.to_le_bytes()); // x pixels per meter
        buf.extend_from_slice(&2835u32.to_le_bytes()); // y pixels per meter
        buf.extend_from_slice(&0u32.to_le_bytes()); // colors used
        buf.extend_from_slice(&0u32.to_le_bytes()); // important colors

        // BMP storage rows are bottom-up for positive heights and top-down for
        // negative heights. Encode the same logical image in either form.
        for storage_row in 0..height {
            let logical_row = if signed_height.is_positive() {
                height - 1 - storage_row
            } else {
                storage_row
            };
            for col in 0..width {
                let [red, green, blue] = expected_rgb(logical_row, col);
                buf.extend_from_slice(&[blue, green, red]);
            }
            // Padding bytes to reach row_stride.
            let padding = row_stride - bytes_per_row;
            buf.extend(std::iter::repeat_n(0u8, padding as usize));
        }

        buf
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "Each component is explicitly reduced modulo 256 before conversion to u8."
    )]
    fn expected_rgb(row: u32, column: u32) -> [u8; RGB_BYTES_PER_PIXEL] {
        [
            (row % 256) as u8,
            (column % 256) as u8,
            ((row + column) % 256) as u8,
        ]
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

    fn pixel_at(pixels: &[u8], row: usize, column: usize) -> Result<[u8; 3], BoxErr> {
        let offset = (row * SCREEN_CAPTURE_WIDTH as usize + column) * RGB_BYTES_PER_PIXEL;
        let end = offset + RGB_BYTES_PER_PIXEL;
        let pixel = pixels.get(offset..end).ok_or_else(|| {
            format!(
                "pixel_at: range {offset}..{end} out of bounds (len={})",
                pixels.len()
            )
        })?;
        <[u8; 3]>::try_from(pixel).map_err(|_| {
            format!(
                "pixel_at: expected exactly three bytes, got {}",
                pixel.len()
            )
            .into()
        })
    }

    fn assert_invalid_bmp(data: &[u8]) -> TestResult {
        let err = parse(data)
            .err()
            .ok_or("expected InvalidBmpHeader but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidBmpHeader { .. }),
            "expected InvalidBmpHeader, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn parse_valid_d75_capture() -> TestResult {
        let bmp = build_bmp(240, 180, 24);
        let cap = parse(&bmp)?;

        assert_eq!(cap.width(), 240);
        assert_eq!(cap.height(), 180);
        assert_eq!(cap.bits_per_pixel(), 24);
        // 240 * 180 * 3 = 129600 canonical RGB bytes.
        assert_eq!(cap.rgb888().len(), SCREEN_CAPTURE_RGB_BYTE_LEN);
        Ok(())
    }

    #[test]
    fn signed_height_forms_have_identical_top_down_rgb() -> TestResult {
        let bottom_up = parse(&build_bmp(240, 180, 24))?;
        let top_down = parse(&build_bmp(240, -180, 24))?;

        assert_eq!(bottom_up, top_down);
        assert_eq!(pixel_at(bottom_up.rgb888(), 0, 0)?, expected_rgb(0, 0));
        assert_eq!(pixel_at(bottom_up.rgb888(), 5, 7)?, expected_rgb(5, 7));
        assert_eq!(
            pixel_at(bottom_up.rgb888(), 179, 239)?,
            expected_rgb(179, 239)
        );
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
        assert_invalid_bmp(&bmp)
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
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 28, &32u16.to_le_bytes())?;
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
    fn undersized_dib_header_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 14, &39u32.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn compressed_bmp_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        // Set compression to 1 (BI_RLE8) at offset 30.
        write_slice(&mut bmp, 30, &1u32.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn non_unit_color_plane_count_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 26, &2u16.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn declared_file_size_mismatch_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        let incorrect_size = u32::try_from(bmp.len())? - 1;
        write_slice(&mut bmp, 2, &incorrect_size.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn nonzero_reserved_header_fields_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 6, &1u16.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn declared_image_size_mismatch_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 34, &(129_600_u32 - 1).to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn pixel_offset_inside_headers_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        write_slice(&mut bmp, 10, &53u32.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn bytes_after_declared_pixel_array_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        bmp.push(0);
        let enlarged_size = u32::try_from(bmp.len())?;
        write_slice(&mut bmp, 2, &enlarged_size.to_le_bytes())?;
        assert_invalid_bmp(&bmp)
    }

    #[test]
    fn zero_and_unrepresentable_heights_rejected() -> TestResult {
        for height in [0, i32::MIN] {
            let mut bmp = build_bmp(240, 180, 24);
            write_slice(&mut bmp, 22, &height.to_le_bytes())?;
            assert_invalid_bmp(&bmp)?;
        }
        Ok(())
    }

    #[test]
    fn truncated_pixel_data_rejected() -> TestResult {
        let mut bmp = build_bmp(240, 180, 24);
        // Truncate to just the header.
        bmp.truncate(60);
        write_slice(&mut bmp, 2, &60u32.to_le_bytes())?;
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
