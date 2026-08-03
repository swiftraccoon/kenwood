//! Reading a patched radio's live memory over CAT.
//!
//! The normal-mode `GM` mnemonic is safe only on a firmware image carrying one
//! exact V1.03 patch. [`Radio::qualify_mem_read_for`] performs a strict,
//! byte-exact attestation and returns a borrowed [`MemoryReader`]. Raw
//! [`Radio::execute`] calls remain unable to send memory reads.

use std::time::Duration;

use crate::error::{Error, ProtocolError, TransportError};
use crate::protocol::memread::{parse_strict_read_reply, plan_read_for_target};
use crate::transport::Transport;
use crate::types::{MemoryReadOffset, MemoryReadTarget, ReadLen};
use crate::verify::StateSnapshot;

use super::{LinkState, McpPhase, Radio};

const EXPECTED_MODEL_FRAME: &[u8] = b"ID TH-D75\r";
const EXPECTED_FIRMWARE_FRAME: &[u8] = b"FV 1.03\r";
const GM_QUIET_WINDOW: Duration = Duration::from_millis(500);
const MAX_GM_FRAME_LEN: usize = 523;

#[derive(Debug, Clone, Copy)]
struct PatchAttestation {
    firmware_offset: u32,
    expected: &'static [u8],
}

/// Main-firmware offset in the CPU-visible low-NOR window.
///
/// DDR maps the running main image at offset zero, while low NOR starts with
/// the 2 MiB bootloader/FLDM window and maps the main image after it.
const LOW_NOR_FIRMWARE_OFFSET: u32 = 0x20_0000;
const BASE_ATTESTATION_FIRMWARE_OFFSET: u32 = 0x06_F8A0;
const DISPATCH_ATTESTATION: &[u8] = &[0x01, 0xEC, 0x02, 0xC0, 0x47, 0x4D, 0x00, 0x00];
const ADAPTER_ATTESTATION: &[u8] = &[
    0x10, 0xB5, 0x14, 0x00, 0x40, 0xF0, 0x0F, 0xFE, 0x02, 0x20, 0x20, 0x70, 0x10, 0xBD,
];
const BOUND_ATTESTATION: &[u8] = &[0x80, 0x26, 0x76, 0x04];
const DDR_BASE_ATTESTATION: &[u8] = &[
    0xC0, 0x26, 0x36, 0x06, 0x01, 0x99, 0x89, 0x19, 0x02, 0xA8, 0x00, 0x9A, 0xA1, 0xF7, 0x8D, 0xFD,
];
const LOW_NOR_BASE_ATTESTATION: &[u8] = &[
    0x60, 0x26, 0x36, 0x06, 0x01, 0x99, 0x89, 0x19, 0x02, 0xA8, 0x00, 0x9A, 0xA1, 0xF7, 0x8D, 0xFD,
];
const COMMON_PATCH_ATTESTATIONS: &[PatchAttestation] = &[
    PatchAttestation {
        firmware_offset: 0x02_E2C8,
        expected: DISPATCH_ATTESTATION,
    },
    PatchAttestation {
        firmware_offset: 0x02_EC00,
        expected: ADAPTER_ATTESTATION,
    },
    PatchAttestation {
        firmware_offset: 0x06_F85C,
        expected: BOUND_ATTESTATION,
    },
];

/// DDR offset containing the fixed screen-capture bitmap header.
pub const DDR_PROBE_OFFSET: MemoryReadOffset = MemoryReadOffset::new_const(0x17_D1BC);

/// Backward-compatible name for [`DDR_PROBE_OFFSET`].
pub const PROBE_OFFSET: MemoryReadOffset = DDR_PROBE_OFFSET;

/// Complete fixed bitmap header at [`DDR_PROBE_OFFSET`].
pub const PROBE_EXPECTED_HEADER: [u8; 54] = [
    0x42, 0x4D, 0x76, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28, 0x00,
    0x00, 0x00, 0xF0, 0x00, 0x00, 0x00, 0xB4, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x40, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// First 16 bytes of [`PROBE_EXPECTED_HEADER`].
pub const PROBE_EXPECTED: [u8; 16] = [
    0x42, 0x4D, 0x76, 0xFA, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28, 0x00,
];

/// Exclusive capability for one attested patched memory target.
///
/// The reader borrows the radio, so reconnecting, entering MCP/TNC modes, or
/// using raw transport I/O requires dropping this capability first. A failed or
/// cancelled read poisons it; reconnect and qualify again before further use.
pub struct MemoryReader<'a, T: Transport> {
    radio: &'a mut Radio<T>,
    target: MemoryReadTarget,
    valid: bool,
}

impl<T: Transport> std::fmt::Debug for MemoryReader<'_, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryReader")
            .field("target", &self.target)
            .field("valid", &self.valid)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> MemoryReader<'_, T> {
    /// Returns the exact backend proved by attestation.
    #[must_use]
    pub const fn target(&self) -> MemoryReadTarget {
        self.target
    }

    /// Reports whether no prior reader operation poisoned this capability.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.valid
    }

    /// Reads `len` bytes at `offset`.
    ///
    /// The request is rejected before I/O unless it is inside this reader's
    /// qualified target. The reply must use the radio's exact uppercase form,
    /// echo the requested offset, and contain exactly the requested byte count.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryReadNotQualified`] after a prior failed or
    /// cancelled operation, [`Error::MemoryReadOutOfRange`] outside the
    /// qualified target, or a transport/protocol error for an invalid exchange.
    pub async fn read_memory(
        &mut self,
        offset: MemoryReadOffset,
        len: ReadLen,
    ) -> Result<Vec<u8>, Error> {
        self.require_valid()?;
        self.validate_range(offset, len)?;

        // These assignments occur before the first await. Dropping this future
        // with a reply in flight therefore leaves both stream and capability
        // poisoned rather than silently reusable.
        self.valid = false;
        self.radio.gm_poisoned = true;
        self.radio.desynced = true;
        let result = self.radio.strict_gm_read(offset, len).await;
        if result.is_ok() {
            self.radio.gm_poisoned = false;
            self.radio.desynced = false;
            self.valid = true;
        }
        result
    }

    /// Reads a contiguous range in requests of at most 256 bytes.
    ///
    /// The complete range is validated before the first request is sent.
    ///
    /// # Errors
    ///
    /// Returns a validation error for a zero or out-of-range request, plus any
    /// error from [`Self::read_memory`].
    pub async fn read_memory_range(
        &mut self,
        start: MemoryReadOffset,
        total: u32,
    ) -> Result<Vec<u8>, Error> {
        self.require_valid()?;
        let chunks = plan_read_for_target(self.target, start, total)?;
        let mut output = Vec::new();
        for chunk in chunks {
            let bytes = self.read_memory(chunk.offset, chunk.len).await?;
            output.extend_from_slice(&bytes);
        }
        Ok(output)
    }

    /// Captures DDR windows in their given order.
    ///
    /// Low-NOR readers reject this operation before I/O because
    /// [`StateSnapshot`] and its runtime offset maps describe mutable DDR.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MemoryReadNotQualified`] for low NOR or a poisoned
    /// reader, plus any error from [`Self::read_memory_range`].
    pub async fn capture_snapshot(
        &mut self,
        windows: &[(MemoryReadOffset, u32)],
    ) -> Result<StateSnapshot, Error> {
        self.require_ddr()?;
        let mut captured = Vec::with_capacity(windows.len());
        for &(offset, length) in windows {
            let bytes = self.read_memory_range(offset, length).await?;
            captured.push((offset, bytes));
        }
        Ok(StateSnapshot::from_windows(captured))
    }

    /// Samples DDR at fixed strides.
    ///
    /// Every sample is planned and target-checked before the first I/O.
    ///
    /// # Errors
    ///
    /// Returns a validation error for a zero stride, zero range, or any sample
    /// crossing the DDR bound. Low-NOR and poisoned readers fail before I/O.
    pub async fn sample_range(
        &mut self,
        start: MemoryReadOffset,
        total: u32,
        stride: u32,
        sample_len: ReadLen,
    ) -> Result<StateSnapshot, Error> {
        self.require_ddr()?;
        if stride == 0 {
            return Err(crate::error::ValidationError::MemoryParamOutOfRange {
                name: "sample stride",
                value: 0,
                detail: "must be at least 1 byte",
            }
            .into());
        }
        if total == 0 {
            return Err(crate::error::ValidationError::MemoryParamOutOfRange {
                name: "sample range length",
                value: 0,
                detail: "must be at least 1 byte",
            }
            .into());
        }

        let end = u64::from(start.as_u32()) + u64::from(total);
        if end > u64::from(self.target.bound()) {
            return Err(Error::MemoryReadOutOfRange {
                target: self.target.as_str(),
                offset: start.as_u32(),
                length: sample_len.as_u16(),
                bound: self.target.bound(),
            });
        }

        let mut planned = Vec::new();
        let mut cursor = start.as_u32();
        while u64::from(cursor) < end {
            let offset = MemoryReadOffset::new(cursor)?;
            self.validate_range(offset, sample_len)?;
            planned.push(offset);
            cursor = match cursor.checked_add(stride) {
                Some(next) => next,
                None => break,
            };
        }

        let mut windows = Vec::with_capacity(planned.len());
        for offset in planned {
            let bytes = self.read_memory(offset, sample_len).await?;
            windows.push((offset, bytes));
        }
        Ok(StateSnapshot::from_windows(windows))
    }

    const fn require_valid(&self) -> Result<(), Error> {
        if self.valid {
            Ok(())
        } else {
            Err(Error::MemoryReadNotQualified)
        }
    }

    fn require_ddr(&self) -> Result<(), Error> {
        self.require_valid()?;
        if self.target == MemoryReadTarget::DdrV103 {
            Ok(())
        } else {
            Err(Error::MemoryReadNotQualified)
        }
    }

    fn validate_range(&self, offset: MemoryReadOffset, len: ReadLen) -> Result<(), Error> {
        let end = u64::from(offset.as_u32()) + u64::from(len.as_u16());
        if end <= u64::from(self.target.bound()) {
            Ok(())
        } else {
            Err(Error::MemoryReadOutOfRange {
                target: self.target.as_str(),
                offset: offset.as_u32(),
                length: len.as_u16(),
                bound: self.target.bound(),
            })
        }
    }
}

impl<T: Transport> Radio<T> {
    /// Prove that a freshly reopened transport returned one exact TH-D75
    /// identity frame. This deliberately bypasses the public GM poison gate
    /// while leaving the poison set until the caller observes success.
    pub(super) async fn prove_reopened_thd75_identity(&mut self) -> Result<(), Error> {
        self.strict_expect(b"ID\r", EXPECTED_MODEL_FRAME).await
    }

    /// Attests one exact V1.03 patch target and returns its exclusive reader.
    ///
    /// The literal first `GM` operation reads the target-specific base
    /// instruction. No target data is read until that byte matches. Every
    /// attestation reply is byte-exact and every checked `GM` is followed by an
    /// exact identity response plus a 500 ms quiet line.
    ///
    /// # Errors
    ///
    /// Returns an error without a capability if the stream is dirty or
    /// desynchronized, or if any identity, patch, data, stability, or boundary
    /// check fails.
    pub async fn qualify_mem_read_for(
        &mut self,
        target: MemoryReadTarget,
    ) -> Result<MemoryReader<'_, T>, Error> {
        self.attest_memory_read(target).await?;
        Ok(MemoryReader {
            radio: self,
            target,
            valid: true,
        })
    }

    /// Explicit-target compatibility spelling for
    /// [`Self::qualify_mem_read_for`].
    ///
    /// # Errors
    ///
    /// Returns any error from [`Self::qualify_mem_read_for`].
    pub async fn probe_mem_read_for(
        &mut self,
        target: MemoryReadTarget,
    ) -> Result<MemoryReader<'_, T>, Error> {
        self.qualify_mem_read_for(target).await
    }

    async fn attest_memory_read(&mut self, target: MemoryReadTarget) -> Result<(), Error> {
        if self.mcp_phase != McpPhase::Inactive {
            return Err(Error::McpInterrupted);
        }
        if self.gm_poisoned {
            return Err(Error::MemoryReadStreamPoisoned);
        }
        if self.desynced {
            return Err(Self::strict_protocol_error(
                "a synchronized CAT stream before GM attestation",
                b"stream is marked desynchronized".to_vec(),
            ));
        }
        if !self.codec.is_empty() {
            return Err(Self::strict_protocol_error(
                "an empty CAT codec before GM attestation",
                b"buffered CAT bytes".to_vec(),
            ));
        }

        // Set before the first await. Cancellation or any failed proof requires
        // a fresh transport; a short ordinary stale-input drain is insufficient.
        self.gm_poisoned = true;
        self.desynced = true;
        let result = async {
            self.require_strict_quiet().await?;
            self.strict_expect(b"ID\r", EXPECTED_MODEL_FRAME).await?;
            self.strict_expect(b"FV\r", EXPECTED_FIRMWARE_FRAME).await?;
            self.require_strict_quiet().await?;
            self.attest_patch_windows(target).await?;
            match target {
                MemoryReadTarget::DdrV103 => self.attest_ddr_v103().await,
                MemoryReadTarget::LowNorV103 => self.attest_low_nor_v103().await,
            }
        }
        .await;
        if result.is_ok() {
            self.gm_poisoned = false;
            self.desynced = false;
        }
        result
    }

    async fn attest_patch_windows(&mut self, target: MemoryReadTarget) -> Result<(), Error> {
        let target_base = match target {
            MemoryReadTarget::DdrV103 => DDR_BASE_ATTESTATION,
            MemoryReadTarget::LowNorV103 => LOW_NOR_BASE_ATTESTATION,
        };
        let base_offset = Self::patch_attestation_offset(target, BASE_ATTESTATION_FIRMWARE_OFFSET);

        // This is deliberately the first GM request.
        let discriminator = target_base.get(..1).ok_or_else(|| {
            Self::strict_protocol_error("a target base attestation byte", target_base.to_vec())
        })?;
        self.attest_exact_read(target, base_offset, discriminator)
            .await?;
        for item in COMMON_PATCH_ATTESTATIONS {
            let offset = Self::patch_attestation_offset(target, item.firmware_offset);
            self.attest_exact_read(target, offset, item.expected)
                .await?;
        }
        self.attest_exact_read(target, base_offset, target_base)
            .await
    }

    async fn attest_ddr_v103(&mut self) -> Result<(), Error> {
        for length in [1_usize, 16, PROBE_EXPECTED_HEADER.len()] {
            self.attest_exact_read(
                MemoryReadTarget::DdrV103,
                DDR_PROBE_OFFSET.as_u32(),
                PROBE_EXPECTED_HEADER.get(..length).ok_or_else(|| {
                    Self::strict_protocol_error(
                        "a valid fixed DDR probe prefix",
                        length.to_string().into_bytes(),
                    )
                })?,
            )
            .await?;
        }

        let full = self
            .attest_checked_read(MemoryReadTarget::DdrV103, 0, ReadLen::MAX)
            .await?;
        Self::require_exact_read_len(&full, 256, "maximum-length DDR read")?;
        let top = self
            .attest_checked_read(MemoryReadTarget::DdrV103, 0xFF_FF00, ReadLen::MAX)
            .await?;
        Self::require_exact_read_len(&top, 256, "top-of-DDR-window read")?;

        let crossing = self
            .strict_cat_exchange(b"GM FFFFFF,02\r", MAX_GM_FRAME_LEN)
            .await?;
        self.strict_checkpoint().await?;
        if crossing == b"N\r" {
            Ok(())
        } else {
            Err(Self::strict_protocol_error(
                "exact N refusal for DDR crossing read",
                crossing,
            ))
        }
    }

    async fn attest_low_nor_v103(&mut self) -> Result<(), Error> {
        let one = self
            .attest_checked_read(MemoryReadTarget::LowNorV103, 0, ReadLen::new(1)?)
            .await?;
        let sixteen = self
            .attest_checked_read(MemoryReadTarget::LowNorV103, 0, ReadLen::new(16)?)
            .await?;
        let sixty_four = self
            .attest_checked_read(MemoryReadTarget::LowNorV103, 0, ReadLen::new(64)?)
            .await?;
        let maximum = self
            .attest_checked_read(MemoryReadTarget::LowNorV103, 0, ReadLen::MAX)
            .await?;
        Self::require_exact_read_len(&maximum, 256, "maximum-length low-NOR read")?;
        if maximum.get(..1) != Some(one.as_slice())
            || maximum.get(..16) != Some(sixteen.as_slice())
            || maximum.get(..64) != Some(sixty_four.as_slice())
        {
            return Err(Self::strict_protocol_error(
                "nested 1/16/64/256-byte low-NOR prefixes",
                maximum,
            ));
        }

        let last = self
            .attest_checked_read(MemoryReadTarget::LowNorV103, 0x1F_FFFF, ReadLen::new(1)?)
            .await?;
        Self::require_exact_read_len(&last, 1, "last qualified low-NOR byte")?;
        for _ in 0..3 {
            let repeated = self
                .attest_checked_read(MemoryReadTarget::LowNorV103, 0, ReadLen::new(16)?)
                .await?;
            if repeated != sixteen {
                return Err(Self::strict_protocol_error(
                    "stable repeated low-NOR prefix",
                    repeated,
                ));
            }
        }
        Ok(())
    }

    async fn attest_exact_read(
        &mut self,
        target: MemoryReadTarget,
        raw_offset: u32,
        expected: &[u8],
    ) -> Result<(), Error> {
        let length = u16::try_from(expected.len()).map_err(|_| {
            Self::strict_protocol_error(
                "an attestation window no larger than 256 bytes",
                expected.len().to_string().into_bytes(),
            )
        })?;
        let actual = self
            .attest_checked_read(target, raw_offset, ReadLen::new(length)?)
            .await?;
        if actual == expected {
            Ok(())
        } else {
            Err(Self::strict_protocol_error(
                &format!("exact bytes at offset 0x{raw_offset:06X}"),
                actual,
            ))
        }
    }

    async fn attest_checked_read(
        &mut self,
        target: MemoryReadTarget,
        raw_offset: u32,
        len: ReadLen,
    ) -> Result<Vec<u8>, Error> {
        if !Self::attestation_read_allowed(target, raw_offset, len) {
            return Err(Self::strict_protocol_error(
                "one exact allowlisted GM attestation read",
                format!("{target:?} 0x{raw_offset:06X}+{}", len.as_u16()).into_bytes(),
            ));
        }
        let bytes = self
            .strict_gm_read(MemoryReadOffset::new(raw_offset)?, len)
            .await?;
        self.strict_checkpoint().await?;
        Ok(bytes)
    }

    fn attestation_read_allowed(target: MemoryReadTarget, raw_offset: u32, len: ReadLen) -> bool {
        let length = len.as_u16();
        let base_offset = Self::patch_attestation_offset(target, BASE_ATTESTATION_FIRMWARE_OFFSET);
        let patch = (raw_offset == base_offset && matches!(length, 1 | 16))
            || COMMON_PATCH_ATTESTATIONS.iter().any(|item| {
                raw_offset == Self::patch_attestation_offset(target, item.firmware_offset)
                    && usize::from(length) == item.expected.len()
            });
        patch
            || match target {
                MemoryReadTarget::DdrV103 => matches!(
                    (raw_offset, length),
                    (0x17_D1BC, 1 | 16 | 54) | (0 | 0xFF_FF00, 256)
                ),
                MemoryReadTarget::LowNorV103 => matches!(
                    (raw_offset, length),
                    (0, 1 | 16 | 64 | 256) | (0x1F_FFFF, 1)
                ),
            }
    }

    /// Resolve one flat main-firmware offset in the selected reader's address
    /// space.
    ///
    /// The DDR reader starts at the running image (`0xC0000000 + offset`).
    /// The low-NOR reader starts at NOR zero and therefore reaches the same
    /// instruction after the 2 MiB bootloader/FLDM prefix.
    const fn patch_attestation_offset(target: MemoryReadTarget, firmware_offset: u32) -> u32 {
        match target {
            MemoryReadTarget::DdrV103 => firmware_offset,
            MemoryReadTarget::LowNorV103 => LOW_NOR_FIRMWARE_OFFSET + firmware_offset,
        }
    }

    pub(super) async fn strict_checkpoint(&mut self) -> Result<(), Error> {
        self.strict_expect(b"ID\r", EXPECTED_MODEL_FRAME).await?;
        self.require_strict_quiet().await
    }

    pub(super) async fn strict_gm_read(
        &mut self,
        offset: MemoryReadOffset,
        len: ReadLen,
    ) -> Result<Vec<u8>, Error> {
        let request = format!("GM {offset},{:02X}\r", len.as_wire());
        let expected_len = 11 + usize::from(len.as_u16()) * 2;
        let frame = self
            .strict_cat_exchange(request.as_bytes(), expected_len)
            .await?;
        parse_strict_read_reply(&frame, offset, len).map_err(Error::Protocol)
    }

    pub(super) async fn strict_expect(
        &mut self,
        request: &[u8],
        expected: &[u8],
    ) -> Result<(), Error> {
        let actual = self.strict_cat_exchange(request, expected.len()).await?;
        if actual == expected {
            Ok(())
        } else {
            Err(Self::strict_protocol_error(
                &format!("exact response {expected:?}"),
                actual,
            ))
        }
    }

    pub(super) async fn strict_cat_exchange(
        &mut self,
        request: &[u8],
        max_frame_len: usize,
    ) -> Result<Vec<u8>, Error> {
        if !self.codec.is_empty() {
            return Err(Self::strict_protocol_error(
                "an empty CAT codec during strict exchange",
                b"buffered CAT bytes".to_vec(),
            ));
        }
        if let Some(last) = self.last_cmd_time {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_millis(5) {
                tokio::time::sleep(Duration::from_millis(5).saturating_sub(elapsed)).await;
            }
        }

        match tokio::time::timeout(self.timeout, self.transport.write(request)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _previous = self.link_state_tx.send_replace(LinkState::Down);
                return Err(Error::Transport(error));
            }
            Err(_elapsed) => {
                let _previous = self.link_state_tx.send_replace(LinkState::Down);
                return Err(Error::Timeout(self.timeout));
            }
        }
        self.last_cmd_time = Some(tokio::time::Instant::now());

        let result =
            tokio::time::timeout(self.timeout, self.read_one_strict_frame(max_frame_len)).await;
        match result {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(error)) => {
                if matches!(error, Error::Transport(_)) {
                    let _previous = self.link_state_tx.send_replace(LinkState::Down);
                }
                Err(error)
            }
            Err(_elapsed) => Err(Error::Timeout(self.timeout)),
        }
    }

    async fn read_one_strict_frame(&mut self, max_frame_len: usize) -> Result<Vec<u8>, Error> {
        let mut frame = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = self
                .transport
                .read(&mut buffer)
                .await
                .map_err(Error::Transport)?;
            if count == 0 {
                return Err(Error::Transport(TransportError::Disconnected(
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "radio disconnected during strict CAT exchange",
                    ),
                )));
            }
            let chunk = buffer.get(..count).ok_or_else(|| {
                Self::strict_protocol_error(
                    "a transport read count inside its buffer",
                    count.to_string().into_bytes(),
                )
            })?;
            if let Some(terminator) = chunk.iter().position(|byte| *byte == b'\r') {
                if terminator + 1 != chunk.len() {
                    return Err(Self::strict_protocol_error(
                        "one response frame with no trailing bytes",
                        chunk.to_vec(),
                    ));
                }
                frame.extend_from_slice(chunk);
                if frame.len() > max_frame_len {
                    return Err(Self::strict_protocol_error(
                        "a response inside its strict length limit",
                        frame,
                    ));
                }
                return Ok(frame);
            }
            frame.extend_from_slice(chunk);
            if frame.len() >= max_frame_len {
                return Err(Self::strict_protocol_error(
                    "a terminated response inside its strict length limit",
                    frame,
                ));
            }
        }
    }

    pub(super) async fn require_strict_quiet(&mut self) -> Result<(), Error> {
        if !self.codec.is_empty() {
            return Err(Self::strict_protocol_error(
                "an empty CAT codec during quiet check",
                b"buffered CAT bytes".to_vec(),
            ));
        }
        let mut byte = [0_u8; 1];
        match tokio::time::timeout(GM_QUIET_WINDOW, self.transport.read(&mut byte)).await {
            Err(_elapsed) => Ok(()),
            Ok(Ok(0)) => {
                let _previous = self.link_state_tx.send_replace(LinkState::Down);
                Err(Error::Transport(TransportError::Disconnected(
                    std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "radio disconnected during strict CAT quiet check",
                    ),
                )))
            }
            Ok(Ok(count)) => {
                let actual = byte.get(..count).map_or_else(Vec::new, <[u8]>::to_vec);
                Err(Self::strict_protocol_error(
                    "a quiet CAT line for 500 ms",
                    actual,
                ))
            }
            Ok(Err(error)) => {
                let _previous = self.link_state_tx.send_replace(LinkState::Down);
                Err(Error::Transport(error))
            }
        }
    }

    fn require_exact_read_len(
        actual: &[u8],
        expected: usize,
        operation: &str,
    ) -> Result<(), Error> {
        if actual.len() == expected {
            Ok(())
        } else {
            Err(Self::strict_protocol_error(
                &format!("{expected} bytes for {operation}"),
                actual.len().to_string().into_bytes(),
            ))
        }
    }

    pub(super) fn strict_protocol_error(expected: &str, actual: Vec<u8>) -> Error {
        Error::Protocol(ProtocolError::UnexpectedResponse {
            expected: expected.to_owned(),
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADAPTER_ATTESTATION, BASE_ATTESTATION_FIRMWARE_OFFSET, BOUND_ATTESTATION,
        DDR_BASE_ATTESTATION, DDR_PROBE_OFFSET, DISPATCH_ATTESTATION, LOW_NOR_BASE_ATTESTATION,
        MemoryReader, PROBE_EXPECTED, PROBE_EXPECTED_HEADER,
    };
    use crate::error::Error;
    use crate::protocol::Command;
    use crate::radio::Radio;
    use crate::transport::MockTransport;
    use crate::types::{MemoryReadOffset, MemoryReadTarget, ReadLen};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn reply(offset: u32, data: &[u8]) -> Vec<u8> {
        let hex = crate::protocol::memread::encode_hex_upper(data);
        format!("GM {offset:06X},{hex}\r").into_bytes()
    }

    fn queue_identity(mock: &mut MockTransport) {
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
    }

    fn queue_checked_read(mock: &mut MockTransport, offset: u32, data: &[u8]) {
        let wire_len = if data.len() == 256 { 0 } else { data.len() };
        let request = format!("GM {offset:06X},{wire_len:02X}\r");
        mock.expect(request.as_bytes(), &reply(offset, data));
        mock.expect(b"ID\r", b"ID TH-D75\r");
    }

    fn queue_patch_attestation(
        mock: &mut MockTransport,
        target: MemoryReadTarget,
        discriminator: u8,
        target_base: &[u8],
    ) {
        let base = Radio::<MockTransport>::patch_attestation_offset(
            target,
            BASE_ATTESTATION_FIRMWARE_OFFSET,
        );
        let dispatch = Radio::<MockTransport>::patch_attestation_offset(target, 0x02_E2C8);
        let adapter = Radio::<MockTransport>::patch_attestation_offset(target, 0x02_EC00);
        let bound = Radio::<MockTransport>::patch_attestation_offset(target, 0x06_F85C);
        queue_checked_read(mock, base, &[discriminator]);
        queue_checked_read(mock, dispatch, DISPATCH_ATTESTATION);
        queue_checked_read(mock, adapter, ADAPTER_ATTESTATION);
        queue_checked_read(mock, bound, BOUND_ATTESTATION);
        queue_checked_read(mock, base, target_base);
    }

    fn queue_low_nor_attestation(mock: &mut MockTransport) {
        mock.pend_when_empty();
        queue_identity(mock);
        queue_patch_attestation(
            mock,
            MemoryReadTarget::LowNorV103,
            0x60,
            LOW_NOR_BASE_ATTESTATION,
        );
        let page: Vec<u8> = (0_u8..=255).collect();
        let one: Vec<u8> = page.iter().copied().take(1).collect();
        let sixteen: Vec<u8> = page.iter().copied().take(16).collect();
        let sixty_four: Vec<u8> = page.iter().copied().take(64).collect();
        queue_checked_read(mock, 0, &one);
        queue_checked_read(mock, 0, &sixteen);
        queue_checked_read(mock, 0, &sixty_four);
        queue_checked_read(mock, 0, &page);
        queue_checked_read(mock, 0x1F_FFFF, &[0x5A]);
        for _ in 0..3 {
            queue_checked_read(mock, 0, &sixteen);
        }
    }

    fn queue_ddr_attestation(mock: &mut MockTransport) {
        mock.pend_when_empty();
        queue_identity(mock);
        queue_patch_attestation(mock, MemoryReadTarget::DdrV103, 0xC0, DDR_BASE_ATTESTATION);
        queue_checked_read(mock, DDR_PROBE_OFFSET.as_u32(), &[0x42]);
        queue_checked_read(mock, DDR_PROBE_OFFSET.as_u32(), &PROBE_EXPECTED);
        queue_checked_read(mock, DDR_PROBE_OFFSET.as_u32(), &PROBE_EXPECTED_HEADER);
        queue_checked_read(mock, 0, &[0_u8; 256]);
        queue_checked_read(mock, 0xFF_FF00, &[0_u8; 256]);
        mock.expect(b"GM FFFFFF,02\r", b"N\r");
        mock.expect(b"ID\r", b"ID TH-D75\r");
    }

    fn direct_reader(
        radio: &mut Radio<MockTransport>,
        target: MemoryReadTarget,
    ) -> MemoryReader<'_, MockTransport> {
        MemoryReader {
            radio,
            target,
            valid: true,
        }
    }

    #[test]
    fn attestation_reads_are_mechanically_allowlisted_per_target() -> TestResult {
        assert!(Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::LowNorV103,
            0x26_F8A0,
            ReadLen::new(16)?,
        ));
        assert!(Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::LowNorV103,
            0x1F_FFFF,
            ReadLen::new(1)?,
        ));
        assert!(!Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::LowNorV103,
            0x20_0000,
            ReadLen::new(1)?,
        ));
        assert!(!Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::DdrV103,
            0x1F_FFFF,
            ReadLen::new(1)?,
        ));
        assert!(Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::DdrV103,
            0x06_F8A0,
            ReadLen::new(16)?,
        ));
        assert!(!Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::DdrV103,
            0x26_F8A0,
            ReadLen::new(16)?,
        ));
        assert!(!Radio::<MockTransport>::attestation_read_allowed(
            MemoryReadTarget::LowNorV103,
            0x06_F8A0,
            ReadLen::new(16)?,
        ));
        Ok(())
    }

    #[test]
    fn patch_attestation_offsets_are_backend_relative() {
        let cases = [
            (0x02_E2C8, 0x02_E2C8, 0x22_E2C8),
            (0x02_EC00, 0x02_EC00, 0x22_EC00),
            (0x06_F85C, 0x06_F85C, 0x26_F85C),
            (BASE_ATTESTATION_FIRMWARE_OFFSET, 0x06_F8A0, 0x26_F8A0),
        ];
        for (firmware_offset, expected_ddr, expected_low_nor) in cases {
            assert_eq!(
                Radio::<MockTransport>::patch_attestation_offset(
                    MemoryReadTarget::DdrV103,
                    firmware_offset,
                ),
                expected_ddr,
                "DDR must address the running main image directly"
            );
            assert_eq!(
                Radio::<MockTransport>::patch_attestation_offset(
                    MemoryReadTarget::LowNorV103,
                    firmware_offset,
                ),
                expected_low_nor,
                "low NOR must retain the 2 MiB bootloader/FLDM prefix"
            );
        }
    }

    #[tokio::test]
    async fn raw_execute_never_sends_gm() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .execute(Command::ReadMemory {
                offset: MemoryReadOffset::ZERO,
                len: ReadLen::new(1)?,
            })
            .await;
        assert!(matches!(result, Err(Error::MemoryReadNotQualified)));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn low_nor_qualification_uses_flash_relative_patch_offsets() -> TestResult {
        let mut mock = MockTransport::new();
        queue_low_nor_attestation(&mut mock);
        mock.expect(b"GM 000010,04\r", &reply(0x10, &[0xDE, 0xAD, 0xBE, 0xEF]));
        let mut radio = Radio::connect(mock).await?;
        let mut reader = radio
            .qualify_mem_read_for(MemoryReadTarget::LowNorV103)
            .await?;
        assert_eq!(reader.target(), MemoryReadTarget::LowNorV103);
        let bytes = reader
            .read_memory(MemoryReadOffset::new(0x10)?, ReadLen::new(4)?)
            .await?;
        assert_eq!(bytes, [0xDE, 0xAD, 0xBE, 0xEF]);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn ddr_qualification_uses_runtime_relative_patch_offsets() -> TestResult {
        let mut mock = MockTransport::new();
        queue_ddr_attestation(&mut mock);
        let mut radio = Radio::connect(mock).await?;
        let reader = radio
            .qualify_mem_read_for(MemoryReadTarget::DdrV103)
            .await?;
        assert_eq!(reader.target(), MemoryReadTarget::DdrV103);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_target_stops_before_target_data_reads() -> TestResult {
        let mut mock = MockTransport::new();
        mock.pend_when_empty();
        queue_identity(&mut mock);
        queue_checked_read(&mut mock, 0x06_F8A0, &[0x60]);
        let mut radio = Radio::connect(mock).await?;
        let result = radio.qualify_mem_read_for(MemoryReadTarget::DdrV103).await;
        assert!(result.is_err(), "wrong base discriminator must fail");
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn qualification_rejects_unobserved_extended_firmware_spelling() -> TestResult {
        let mut mock = MockTransport::new();
        mock.pend_when_empty();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03.000\r");
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .qualify_mem_read_for(MemoryReadTarget::LowNorV103)
            .await;
        assert!(
            result.is_err(),
            "strict hardware gate must accept only observed FV bytes"
        );
        Ok(())
    }

    #[tokio::test]
    async fn low_nor_range_is_rejected_before_wire_io() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
        let result = reader
            .read_memory(MemoryReadOffset::new(0x20_0000)?, ReadLen::new(1)?)
            .await;
        assert!(matches!(result, Err(Error::MemoryReadOutOfRange { .. })));
        assert!(reader.is_valid(), "preflight rejection must not poison");
        Ok(())
    }

    #[tokio::test]
    async fn low_nor_last_byte_is_inside_the_qualified_window() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM 1FFFFF,01\r", &reply(0x1F_FFFF, &[0x5A]));
        let mut radio = Radio::connect(mock).await?;
        let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
        let bytes = reader
            .read_memory(MemoryReadOffset::new(0x1F_FFFF)?, ReadLen::new(1)?)
            .await?;
        assert_eq!(bytes, [0x5A]);
        Ok(())
    }

    #[tokio::test]
    async fn low_nor_cannot_create_a_ddr_snapshot() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
        let result = reader
            .capture_snapshot(&[(MemoryReadOffset::ZERO, 1)])
            .await;
        assert!(matches!(result, Err(Error::MemoryReadNotQualified)));
        Ok(())
    }

    #[tokio::test]
    async fn strict_reader_rejects_lowercase_and_poisons_capability() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM 000010,02\r", b"GM 000010,dead\r");
        let mut radio = Radio::connect(mock).await?;
        let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
        let result = reader
            .read_memory(MemoryReadOffset::new(0x10)?, ReadLen::new(2)?)
            .await;
        assert!(result.is_err(), "lowercase strict reply must fail");
        assert!(!reader.is_valid(), "failed I/O must poison the reader");
        Ok(())
    }

    #[tokio::test]
    async fn strict_reader_rejects_two_frames_in_one_read() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM 000010,01\r", b"GM 000010,AA\rID TH-D75\r");
        let mut radio = Radio::connect(mock).await?;
        let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
        let result = reader
            .read_memory(MemoryReadOffset::new(0x10)?, ReadLen::new(1)?)
            .await;
        assert!(result.is_err(), "a trailing frame must fail");
        Ok(())
    }

    #[tokio::test]
    async fn strict_reader_rejects_a_wrong_echo() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"GM 000010,01\r", &reply(0x11, &[0xAA]));
        let mut radio = Radio::connect(mock).await?;
        let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
        let result = reader
            .read_memory(MemoryReadOffset::new(0x10)?, ReadLen::new(1)?)
            .await;
        assert!(result.is_err(), "wrong echoed offset must fail");
        assert!(!reader.is_valid(), "failed strict exchange must poison");
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_qualification_marks_the_stream_desynchronized() -> TestResult {
        let mut mock = MockTransport::new();
        mock.pend_when_empty();
        let mut radio = Radio::connect(mock).await?;
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(1),
            radio.qualify_mem_read_for(MemoryReadTarget::LowNorV103),
        )
        .await;
        assert!(result.is_err(), "outer cancellation should win");
        assert!(
            radio.desynced,
            "cancelled qualification must poison the CAT stream"
        );
        let ordinary = radio.identify().await;
        assert!(
            matches!(ordinary, Err(Error::MemoryReadStreamPoisoned)),
            "ordinary CAT must remain blocked until transport reopen: {ordinary:?}"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_reader_operation_poisons_the_capability() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_hang(b"GM 000010,01\r");
        let mut radio = Radio::connect(mock).await?;
        {
            let mut reader = direct_reader(&mut radio, MemoryReadTarget::LowNorV103);
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(1),
                reader.read_memory(MemoryReadOffset::new(0x10)?, ReadLen::new(1)?),
            )
            .await;
            assert!(result.is_err(), "outer cancellation should win");
            assert!(
                !reader.is_valid(),
                "cancelled reader operation must poison the capability"
            );
        }
        assert!(
            radio.desynced,
            "cancelled reader operation must poison the CAT stream"
        );
        let ordinary = radio.identify().await;
        assert!(
            matches!(ordinary, Err(Error::MemoryReadStreamPoisoned)),
            "ordinary CAT must remain blocked until transport reopen: {ordinary:?}"
        );
        let raw = radio.transport_write(b"ID\r").await;
        assert!(
            matches!(raw, Err(Error::MemoryReadStreamPoisoned)),
            "raw transport writes must remain blocked until reopen: {raw:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn buffered_codec_bytes_block_attestation_without_a_write() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        radio.codec.feed(b"partial");
        let result = radio
            .qualify_mem_read_for(MemoryReadTarget::LowNorV103)
            .await;
        assert!(result.is_err(), "buffered CAT data must block proof");
        assert!(
            !radio.desynced,
            "preflight rejection before I/O need not poison the stream"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_would_block_is_not_a_quiet_line() -> TestResult {
        let mock = MockTransport::new();
        let mut radio = Radio::connect(mock).await?;
        let result = radio
            .qualify_mem_read_for(MemoryReadTarget::LowNorV103)
            .await;
        assert!(result.is_err(), "only a full quiet timeout may pass");
        Ok(())
    }
}
