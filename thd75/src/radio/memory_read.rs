//! Reading the running radio's memory over CAT.
//!
//! # Firmware requirement
//!
//! These methods require firmware modified by the `thd75-fw` project. On an
//! unmodified radio the mnemonic they use performs an unrelated function, so
//! [`Radio::probe_mem_read`] exists to fail closed before any caller trusts a
//! read. Probe first; do not assume support.
//!
//! # Why this exists
//!
//! An MCP page read-back proves bytes reached configuration flash. It does not
//! prove the running radio applied them, because programming mode drops the USB
//! connection on exit and the radio reloads state on its own schedule. Reading
//! live memory closes that gap: snapshot, change the setting, snapshot again,
//! and the diff is evidence about the running radio rather than about flash.

use crate::error::{Error, ProtocolError};
use crate::protocol::memread::plan_read;
use crate::protocol::{Command, Response};
use crate::transport::Transport;
use crate::types::{DdrOffset, ReadLen};
use crate::verify::{ByteChange, StateSnapshot};

use super::Radio;

/// Offset the capability probe reads.
///
/// This is the screen-capture BMP header template, chosen because its contents
/// are fixed for a given firmware build and independently corroborated: the
/// same geometry is validated by [`crate::sdcard::capture`].
pub const PROBE_OFFSET: DdrOffset = match DdrOffset::new(0x17_D1BC) {
    Ok(offset) => offset,
    // `Result::unwrap` is not yet const, so this is the const-compatible form.
    // The literal is inside the valid range, so this arm is unreachable and a
    // regression would be a compile-time error rather than a runtime panic.
    Err(_) => unreachable!(),
};

/// The complete 54-byte BMP file header at [`PROBE_OFFSET`].
///
/// Every field is explicable, which is what makes it a good qualification
/// target: `BM` magic, file size `0x1FA76` (54 header plus 129,600 pixel
/// bytes), reserved zeros, pixel offset 54, DIB size 40, width 240, height 180,
/// one plane, 24 bits per pixel, `BI_RGB`, image size `0x1FA40`, zero pixels
/// per metre, zero palette entries. The same geometry is validated by
/// [`crate::sdcard::capture`].
pub const PROBE_EXPECTED_HEADER: [u8; 54] = [
    0x42, 0x4D, 0x76, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28, 0x00,
    0x00, 0x00, 0xF0, 0x00, 0x00, 0x00, 0xB4, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x40, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// The bytes expected at [`PROBE_OFFSET`].
///
/// A BMP file header for a 240 by 180 24-bit image: the `BM` magic, file size
/// `0x1FA76` (54 header bytes plus 129,600 pixel bytes), reserved zeros, pixel
/// data offset 54, and DIB header size 40.
pub const PROBE_EXPECTED: [u8; 16] = [
    0x42, 0x4D, 0x76, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28, 0x00,
];

impl<T: Transport> Radio<T> {
    /// Reads `len` bytes at `offset`.
    ///
    /// The radio echoes the requested offset in its reply, and this checks the
    /// echo against the request so a stale or mis-routed answer becomes an
    /// error instead of silently wrong data.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the reply is not a memory-read reply or
    /// echoes a different offset, plus any error [`Radio::execute`] produces.
    pub async fn read_memory(&mut self, offset: DdrOffset, len: ReadLen) -> Result<Vec<u8>, Error> {
        let response = self.execute(Command::ReadMemory { offset, len }).await?;
        match response {
            Response::MemoryData {
                offset: echoed,
                bytes,
            } => {
                if echoed == offset {
                    Ok(bytes)
                } else {
                    Err(Error::Protocol(ProtocolError::FieldParse {
                        command: crate::protocol::memread::MEM_READ_MNEMONIC.to_owned(),
                        field: "offset".to_owned(),
                        detail: format!("requested {offset}, radio echoed {echoed}"),
                    }))
                }
            }
            other => Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("memory data at {offset}"),
                actual: format!("{other:?}").into_bytes(),
            })),
        }
    }

    /// Reads `total` bytes starting at `start`, in requests of at most 256.
    ///
    /// The whole range is validated against the radio's bound before the first
    /// request is sent, so this either sends only acceptable requests or sends
    /// none at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] if `total` is zero or the range crosses
    /// the addressable bound, plus any error [`Radio::read_memory`] produces.
    pub async fn read_memory_range(
        &mut self,
        start: DdrOffset,
        total: u32,
    ) -> Result<Vec<u8>, Error> {
        let chunks = plan_read(start, total)?;
        let mut out = Vec::with_capacity(total as usize);
        for chunk in chunks {
            let bytes = self.read_memory(chunk.offset, chunk.len).await?;
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    /// Runs the full post-flash qualification for the memory reader.
    ///
    /// This is what should be executed first on a freshly modified radio. It
    /// escalates the read size, then performs the one check whose polarity is
    /// inverted and therefore easy to get wrong by hand: a request that
    /// overruns the radio's accepted window **must be refused**, and a refusal
    /// is the passing outcome.
    ///
    /// The escalation exists because the failure modes differ by size. A
    /// one-byte read proves the command is reachable at all. Sixteen and
    /// fifty-four bytes prove the base address is right, since a wrong base
    /// would return real but unrelated memory. Two hundred and fifty-six
    /// proves the length field's `0x00` means 256 rather than zero.
    ///
    /// A widened bound that never rejects anything is indistinguishable from a
    /// correct one by reading alone, which is why the last two steps are not
    /// optional. Together they bracket the boundary: the highest aligned
    /// request that must succeed, and the one just past it that must not.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if any read returns unexpected bytes, or if
    /// the deliberately out-of-range request is **accepted**, which means the
    /// bound was not applied. Also returns whatever [`Radio::read_memory`]
    /// produces for a genuine transport or protocol failure.
    pub async fn qualify_mem_read(&mut self) -> Result<(), Error> {
        // Step 1: reachable at all.
        let one = self.read_memory(PROBE_OFFSET, ReadLen::new(1)?).await?;
        Self::expect_prefix(&one, 1)?;
        tracing::info!("qualify: 1-byte read matches");

        // Steps 2 and 3: the base address is correct.
        let sixteen = self.read_memory(PROBE_OFFSET, ReadLen::new(16)?).await?;
        Self::expect_prefix(&sixteen, 16)?;
        tracing::info!("qualify: 16-byte read matches");

        let header = self.read_memory(PROBE_OFFSET, ReadLen::new(54)?).await?;
        Self::expect_prefix(&header, 54)?;
        tracing::info!("qualify: full 54-byte BMP header matches");

        // Step 4: `0x00` on the wire means 256, not zero.
        let full = self.read_memory(DdrOffset::ZERO, ReadLen::MAX).await?;
        if full.len() != 256 {
            return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "256 bytes for a maximum-length read".to_owned(),
                actual: format!("{} bytes", full.len()).into_bytes(),
            }));
        }
        tracing::info!("qualify: maximum-length read returned 256 bytes");

        // Step 5: the highest request that must still succeed.
        let top = DdrOffset::new(0x00FF_FF00)?;
        let last_ok = self.read_memory(top, ReadLen::MAX).await?;
        if last_ok.len() != 256 {
            return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "256 bytes at the top of the window".to_owned(),
                actual: format!("{} bytes", last_ok.len()).into_bytes(),
            }));
        }
        tracing::info!("qualify: read ending exactly at the bound succeeded");

        // Step 6: inverted polarity. This request overruns the window by one
        // byte and must be refused. Success here means the bound is not being
        // applied, which is a failed qualification however healthy the earlier
        // reads looked.
        if let Ok(bytes) = self.read_memory(DdrOffset::MAX, ReadLen::MAX).await {
            return Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: "a refusal for a read that overruns the accepted window".to_owned(),
                actual: format!(
                    "the radio accepted it and returned {} bytes, so the bound is \
                     not being enforced",
                    bytes.len()
                )
                .into_bytes(),
            }));
        }
        tracing::info!("qualify: out-of-range read was refused, as required");
        Ok(())
    }

    /// Compares `actual` against the first `len` bytes of the known header.
    fn expect_prefix(actual: &[u8], len: usize) -> Result<(), Error> {
        let expected = PROBE_EXPECTED_HEADER.get(..len).unwrap_or(&[]);
        if actual == expected {
            Ok(())
        } else {
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("{expected:02X?} at {PROBE_OFFSET}"),
                actual: actual.to_vec(),
            }))
        }
    }

    /// Captures the given windows as a single snapshot.
    ///
    /// Each window is an offset and a byte count. Windows are read in the
    /// order given, and the resulting snapshot records them in that order, so
    /// two snapshots taken with the same window list are directly comparable.
    ///
    /// # Errors
    ///
    /// Returns any error [`Radio::read_memory_range`] produces.
    pub async fn capture_snapshot(
        &mut self,
        windows: &[(DdrOffset, u32)],
    ) -> Result<StateSnapshot, Error> {
        let mut captured = Vec::with_capacity(windows.len());
        for &(offset, len) in windows {
            let bytes = self.read_memory_range(offset, len).await?;
            captured.push((offset, bytes));
        }
        Ok(StateSnapshot::from_windows(captured))
    }

    /// Samples a wide range sparsely, for locating large structures.
    ///
    /// Reads `sample_len` bytes at every `stride` bytes across `total`, and
    /// returns them as one snapshot whose windows are the sample points. Two
    /// such snapshots diff exactly like dense ones.
    ///
    /// This exists because dense scanning does not scale to the whole window.
    /// Reading 16 MiB densely is 65,536 requests; sampling 16 bytes every 4 KiB
    /// is 4,096, a factor of sixteen fewer, and it still finds anything large.
    /// A full-screen redraw dirties tens of consecutive kilobytes, so it cannot
    /// hide between sample points. Something small can, which is the trade:
    /// use this to narrow, then scan the candidate densely.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] if `stride` is zero or a sample would
    /// cross the addressable bound, plus any error [`Radio::read_memory`]
    /// produces.
    pub async fn sample_range(
        &mut self,
        start: DdrOffset,
        total: u32,
        stride: u32,
        sample_len: ReadLen,
    ) -> Result<StateSnapshot, Error> {
        if stride == 0 {
            return Err(Error::Validation(
                crate::error::ValidationError::MemoryParamOutOfRange {
                    name: "sample stride",
                    value: 0,
                    detail: "must be at least 1 byte",
                },
            ));
        }
        let mut windows = Vec::new();
        let mut cursor = start.as_u32();
        let end = u64::from(start.as_u32()) + u64::from(total);
        while u64::from(cursor) < end {
            let offset = DdrOffset::new(cursor)?;
            let bytes = self.read_memory(offset, sample_len).await?;
            windows.push((offset, bytes));
            match cursor.checked_add(stride) {
                Some(next) => cursor = next,
                None => break,
            }
        }
        Ok(StateSnapshot::from_windows(windows))
    }

    /// Discovers where a setting lives in memory by changing it and observing
    /// what moved.
    ///
    /// Captures `windows`, runs `mutate`, captures them again, and reports the
    /// bytes that differ. This is the empirical alternative to reverse
    /// engineering each field's location: change a setting through whatever
    /// path already works, and the diff names the address.
    ///
    /// The mutation runs between the two captures and receives the same radio,
    /// so it can use any command, including one that leaves and re-enters
    /// programming mode.
    ///
    /// A change that reports no differing bytes is a real result worth acting
    /// on. It means the setting is not inside the windows given, or that the
    /// write did not take effect on the running radio, and distinguishing
    /// those two is the point of the exercise.
    ///
    /// # Errors
    ///
    /// Returns any error the captures or `mutate` produce, and
    /// [`Error::Verify`] if the two captures somehow disagree on layout.
    pub async fn discover_field(
        &mut self,
        windows: &[(DdrOffset, u32)],
        mutate: impl AsyncFnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<Vec<ByteChange>, Error> {
        let before = self.capture_snapshot(windows).await?;
        mutate(self).await?;
        let after = self.capture_snapshot(windows).await?;
        Ok(before.diff(&after)?)
    }

    /// Confirms the radio supports memory reads and answers correctly.
    ///
    /// Reads a known-constant location and compares byte for byte. A match
    /// establishes in one round trip that the command is present, the radio's
    /// base address is what this code assumes, the offset field is
    /// hexadecimal, the bounds permit that address, and the firmware is the
    /// expected build. Call this before trusting any other read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the bytes do not match
    /// [`PROBE_EXPECTED`], which is the expected outcome on unmodified
    /// firmware, plus any error [`Radio::read_memory`] produces.
    pub async fn probe_mem_read(&mut self) -> Result<(), Error> {
        let len = ReadLen::new(16)?;
        let bytes = self.read_memory(PROBE_OFFSET, len).await?;
        if bytes.as_slice() == PROBE_EXPECTED.as_slice() {
            tracing::debug!("memory-read capability probe passed");
            Ok(())
        } else {
            Err(Error::Protocol(ProtocolError::UnexpectedResponse {
                expected: format!("{PROBE_EXPECTED:02X?} at {PROBE_OFFSET}"),
                actual: bytes,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PROBE_EXPECTED, PROBE_EXPECTED_HEADER, PROBE_OFFSET};
    use crate::protocol::Command;
    use crate::radio::Radio;
    use crate::transport::MockTransport;
    use crate::types::{AfGainLevel, Band, DdrOffset, ReadLen};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Builds the reply the radio would send for a read at `offset`.
    fn reply(offset: u32, data: &[u8]) -> Vec<u8> {
        let hex = crate::protocol::memread::encode_hex_upper(data);
        format!("GM {offset:06X},{hex}\r").into_bytes()
    }

    #[tokio::test]
    async fn read_memory_decodes_bytes() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM 000010,04\r", &reply(0x10, &[0xDE, 0xAD, 0xBE, 0xEF]));
        let mut radio = Radio::connect(mock).await?;
        let bytes = radio
            .read_memory(DdrOffset::new(0x10)?, ReadLen::new(4)?)
            .await?;
        assert_eq!(bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        Ok(())
    }

    #[tokio::test]
    async fn read_memory_rejects_a_mismatched_echo() -> TestResult {
        let mut mock = MockTransport::new();
        // The radio answers with a different offset than was requested.
        mock.expect(b"GM 000010,02\r", &reply(0x20, &[1, 2]));
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .read_memory(DdrOffset::new(0x10)?, ReadLen::new(2)?)
            .await;
        assert!(
            result.is_err(),
            "a mismatched echo must be an error, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn probe_succeeds_on_exact_match() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(
            b"GM 17D1BC,10\r",
            &reply(PROBE_OFFSET.as_u32(), &PROBE_EXPECTED),
        );
        let mut radio = Radio::connect(mock).await?;
        radio.probe_mem_read().await?;
        Ok(())
    }

    #[tokio::test]
    async fn probe_fails_closed_on_single_byte_mismatch() -> TestResult {
        let mut corrupted = PROBE_EXPECTED;
        // Corrupt the 'M' of the BM magic.
        corrupted[1] = 0x00;
        let mut mock = MockTransport::new();
        mock.expect(b"GM 17D1BC,10\r", &reply(PROBE_OFFSET.as_u32(), &corrupted));
        let mut radio = Radio::connect(mock).await?;
        let result = radio.probe_mem_read().await;
        assert!(
            result.is_err(),
            "the probe must fail closed on mismatch, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_memory_range_concatenates_chunks() -> TestResult {
        let mut mock = MockTransport::new();
        // 300 bytes spans two requests: 256 then 44.
        let first: Vec<u8> = (0u8..=255).collect();
        let second: Vec<u8> = vec![0xAA; 44];
        mock.expect(b"GM 000000,00\r", &reply(0, &first));
        mock.expect(b"GM 000100,2C\r", &reply(0x100, &second));
        let mut radio = Radio::connect(mock).await?;
        let bytes = radio.read_memory_range(DdrOffset::ZERO, 300).await?;
        assert_eq!(bytes.len(), 300);
        assert_eq!(bytes.get(299).copied(), Some(0xAA));
        Ok(())
    }

    #[tokio::test]
    async fn discover_field_reports_the_byte_that_moved() -> TestResult {
        let mut mock = MockTransport::new();
        // Snapshot, mutate, snapshot. The mutation here is a plain CAT command
        // whose own reply is consumed before the second capture.
        mock.expect(b"GM 001000,04\r", &reply(0x1000, &[0x11, 0x22, 0x33, 0x44]));
        mock.expect(b"AG 015\r", b"AG 015\r");
        mock.expect(b"GM 001000,04\r", &reply(0x1000, &[0x11, 0x22, 0x99, 0x44]));

        let mut radio = Radio::connect(mock).await?;
        let windows = [(DdrOffset::new(0x1000)?, 4)];
        let changes = radio
            .discover_field(&windows, async |r| {
                let _response = r
                    .execute(Command::SetAfGain {
                        band: Band::A,
                        level: AfGainLevel::new(15),
                    })
                    .await?;
                Ok(())
            })
            .await?;

        assert_eq!(changes.len(), 1);
        let change = changes.first().ok_or("missing change")?;
        assert_eq!(change.offset.as_u32(), 0x1002);
        assert_eq!(change.before, 0x33);
        assert_eq!(change.after, 0x99);
        Ok(())
    }

    #[tokio::test]
    async fn discover_field_reports_nothing_when_the_window_is_wrong() -> TestResult {
        // A silent result is a real finding: either the setting is elsewhere,
        // or the write never reached the running radio.
        let mut mock = MockTransport::new();
        mock.expect(b"GM 002000,02\r", &reply(0x2000, &[0xAA, 0xBB]));
        mock.expect(b"AG 015\r", b"AG 015\r");
        mock.expect(b"GM 002000,02\r", &reply(0x2000, &[0xAA, 0xBB]));

        let mut radio = Radio::connect(mock).await?;
        let windows = [(DdrOffset::new(0x2000)?, 2)];
        let changes = radio
            .discover_field(&windows, async |r| {
                let _response = r
                    .execute(Command::SetAfGain {
                        band: Band::A,
                        level: AfGainLevel::new(15),
                    })
                    .await?;
                Ok(())
            })
            .await?;

        assert!(changes.is_empty(), "expected no changes, got {changes:?}");
        Ok(())
    }

    /// Queues the five reads a passing qualification performs, leaving the
    /// sixth (the out-of-range probe) for the caller to define.
    fn queue_passing_qualification(mock: &mut MockTransport) {
        let header = &PROBE_EXPECTED_HEADER;
        mock.expect(b"GM 17D1BC,01\r", &reply(0x17_D1BC, &header[..1]));
        mock.expect(b"GM 17D1BC,10\r", &reply(0x17_D1BC, &header[..16]));
        mock.expect(b"GM 17D1BC,36\r", &reply(0x17_D1BC, &header[..54]));
        mock.expect(b"GM 000000,00\r", &reply(0, &[0u8; 256]));
        mock.expect(b"GM FFFF00,00\r", &reply(0xFF_FF00, &[0u8; 256]));
    }

    #[tokio::test]
    async fn qualification_passes_when_the_bound_is_enforced() -> TestResult {
        let mut mock = MockTransport::new();
        queue_passing_qualification(&mut mock);
        // The radio refuses the overrunning request, which is the pass.
        mock.expect(b"GM FFFFFF,00\r", b"?\r");

        let mut radio = Radio::connect(mock).await?;
        radio.qualify_mem_read().await?;
        Ok(())
    }

    #[tokio::test]
    async fn qualification_fails_when_an_out_of_range_read_is_accepted() -> TestResult {
        let mut mock = MockTransport::new();
        queue_passing_qualification(&mut mock);
        // The radio answers a request it should have refused. Every earlier
        // step passed, so this is the only signal that the bound is wrong.
        mock.expect(b"GM FFFFFF,00\r", &reply(0xFF_FFFF, &[0u8; 256]));

        let mut radio = Radio::connect(mock).await?;
        let result = radio.qualify_mem_read().await;
        assert!(
            result.is_err(),
            "an accepted out-of-range read must fail qualification, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn qualification_fails_on_a_wrong_base_address() -> TestResult {
        // A wrong base returns real memory that is simply not the header. The
        // one-byte step is too weak to catch it; the 16-byte step is not.
        let mut mock = MockTransport::new();
        mock.expect(b"GM 17D1BC,01\r", &reply(0x17_D1BC, &[0x42]));
        mock.expect(b"GM 17D1BC,10\r", &reply(0x17_D1BC, &[0x42; 16]));

        let mut radio = Radio::connect(mock).await?;
        let result = radio.qualify_mem_read().await;
        assert!(
            result.is_err(),
            "a wrong base must fail qualification, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn sample_range_walks_by_stride() -> TestResult {
        let mut mock = MockTransport::new();
        // 0x300 bytes sampled 2 at a time every 0x100 gives three points.
        mock.expect(b"GM 000000,02\r", &reply(0, &[1, 2]));
        mock.expect(b"GM 000100,02\r", &reply(0x100, &[3, 4]));
        mock.expect(b"GM 000200,02\r", &reply(0x200, &[5, 6]));

        let mut radio = Radio::connect(mock).await?;
        let snapshot = radio
            .sample_range(DdrOffset::ZERO, 0x300, 0x100, ReadLen::new(2)?)
            .await?;

        assert_eq!(snapshot.windows().len(), 3);
        let third = snapshot.windows().get(2).ok_or("missing sample")?;
        assert_eq!(third.0.as_u32(), 0x200);
        Ok(())
    }

    #[tokio::test]
    async fn sample_range_rejects_a_zero_stride() -> TestResult {
        // A zero stride would loop forever issuing the same request.
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .sample_range(DdrOffset::ZERO, 0x100, 0, ReadLen::new(2)?)
            .await;
        assert!(
            result.is_err(),
            "zero stride must be refused, got {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn capture_snapshot_records_windows_in_order() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM 000010,02\r", &reply(0x10, &[1, 2]));
        mock.expect(b"GM 000020,02\r", &reply(0x20, &[3, 4]));

        let mut radio = Radio::connect(mock).await?;
        let windows = [(DdrOffset::new(0x10)?, 2), (DdrOffset::new(0x20)?, 2)];
        let snapshot = radio.capture_snapshot(&windows).await?;

        assert_eq!(snapshot.windows().len(), 2);
        let first = snapshot.windows().first().ok_or("missing window")?;
        let second = snapshot.windows().get(1).ok_or("missing window")?;
        assert_eq!(first.0.as_u32(), 0x10);
        assert_eq!(first.1, vec![1, 2]);
        assert_eq!(second.0.as_u32(), 0x20);
        assert_eq!(second.1, vec![3, 4]);
        Ok(())
    }

    #[tokio::test]
    async fn read_memory_range_rejects_a_range_past_the_bound() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .read_memory_range(DdrOffset::new(0xFF_FF00)?, 257)
            .await;
        assert!(
            result.is_err(),
            "a range past the bound must be refused before sending, got {result:?}"
        );
        Ok(())
    }
}
