use crate::{CodecError, CodecReader, CodecWriter, LengthKind, varint::PartialVarInt};

/// Largest frame body permitted by Minecraft's three-byte length prefix.
pub const MINECRAFT_MAX_FRAME_SIZE: usize = (1 << 21) - 1;
pub const DEFAULT_MAX_FRAME_SIZE: usize = MINECRAFT_MAX_FRAME_SIZE;
/// Default aggregate fragmented-input bound (8 MiB).
pub const DEFAULT_MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAME_LENGTH_PREFIX_BYTES: usize = 3;

/// Independent bounds for one frame body and accumulated fragmented input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    max_frame_size: usize,
    max_buffered_bytes: usize,
}

impl FrameLimits {
    pub fn new(max_frame_size: usize, max_buffered_bytes: usize) -> Result<Self, CodecError> {
        if max_frame_size > MINECRAFT_MAX_FRAME_SIZE {
            return Err(CodecError::FrameTooLong {
                length: max_frame_size,
                max: MINECRAFT_MAX_FRAME_SIZE,
            });
        }
        let minimum_buffer = max_frame_size
            .checked_add(MAX_FRAME_LENGTH_PREFIX_BYTES)
            .ok_or(CodecError::ValueOutOfRange {
                context: "frame buffer limit",
                value: max_frame_size as i128,
                min: 0,
                max: usize::MAX as i128,
            })?;
        if max_buffered_bytes < minimum_buffer {
            return Err(CodecError::ValueOutOfRange {
                context: "maximum buffered frame bytes",
                value: max_buffered_bytes as i128,
                min: minimum_buffer as i128,
                max: usize::MAX as i128,
            });
        }
        Ok(Self {
            max_frame_size,
            max_buffered_bytes,
        })
    }

    #[must_use]
    pub const fn max_frame_size(self) -> usize {
        self.max_frame_size
    }

    #[must_use]
    pub const fn max_buffered_bytes(self) -> usize {
        self.max_buffered_bytes
    }
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_buffered_bytes: DEFAULT_MAX_BUFFERED_BYTES,
        }
    }
}

/// Incremental, transport-independent decoder for uncompressed packet frames.
#[derive(Debug)]
pub struct FrameDecoder {
    limits: FrameLimits,
    buffer: Vec<u8>,
    read_offset: usize,
}

impl FrameDecoder {
    #[must_use]
    pub const fn new(limits: FrameLimits) -> Self {
        Self {
            limits,
            buffer: Vec::new(),
            read_offset: 0,
        }
    }

    /// Appends an arbitrary transport fragment after enforcing the buffer bound.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), CodecError> {
        self.compact_if_useful();
        let buffered =
            self.pending_len()
                .checked_add(bytes.len())
                .ok_or(CodecError::FrameBufferTooLong {
                    buffered: usize::MAX,
                    max: self.limits.max_buffered_bytes(),
                })?;
        if buffered > self.limits.max_buffered_bytes() {
            return Err(CodecError::FrameBufferTooLong {
                buffered,
                max: self.limits.max_buffered_bytes(),
            });
        }
        self.buffer
            .try_reserve(bytes.len())
            .map_err(|_| CodecError::AllocationFailed {
                context: "frame input buffer",
                requested: buffered,
            })?;
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Returns one complete frame body, or `None` when more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, CodecError> {
        let pending = self.pending();
        let (frame_length, prefix_bytes) = match crate::varint::decode_partial_var_int(pending) {
            PartialVarInt::NeedMore => return Ok(None),
            PartialVarInt::Malformed => {
                return Err(CodecError::MalformedLengthPrefix {
                    kind: LengthKind::Frame,
                });
            }
            PartialVarInt::Complete { value, bytes } => (value, bytes),
        };
        if frame_length < 0 {
            return Err(CodecError::NegativeLength {
                kind: LengthKind::Frame,
                value: frame_length,
            });
        }
        if prefix_bytes > MAX_FRAME_LENGTH_PREFIX_BYTES {
            return Err(CodecError::MalformedLengthPrefix {
                kind: LengthKind::Frame,
            });
        }
        let frame_length =
            usize::try_from(frame_length).map_err(|_| CodecError::ValueOutOfRange {
                context: "frame length",
                value: i128::from(frame_length),
                min: 0,
                max: self.limits.max_frame_size() as i128,
            })?;
        if frame_length > self.limits.max_frame_size() {
            return Err(CodecError::FrameTooLong {
                length: frame_length,
                max: self.limits.max_frame_size(),
            });
        }
        let total_length =
            prefix_bytes
                .checked_add(frame_length)
                .ok_or(CodecError::ValueOutOfRange {
                    context: "complete frame length",
                    value: frame_length as i128,
                    min: 0,
                    max: usize::MAX as i128,
                })?;
        if pending.len() < total_length {
            return Ok(None);
        }
        let body = pending
            .get(prefix_bytes..total_length)
            .ok_or(CodecError::UnexpectedEnd {
                context: "frame body",
                needed: frame_length,
                remaining: pending.len().saturating_sub(prefix_bytes),
            })?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(frame_length)
            .map_err(|_| CodecError::AllocationFailed {
                context: "completed frame body",
                requested: frame_length,
            })?;
        output.extend_from_slice(body);
        self.read_offset += total_length;
        if self.read_offset == self.buffer.len() {
            self.buffer.clear();
            self.read_offset = 0;
        }
        Ok(Some(output))
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.pending_len()
    }

    fn pending(&self) -> &[u8] {
        self.buffer.get(self.read_offset..).unwrap_or_default()
    }

    fn pending_len(&self) -> usize {
        self.buffer.len().saturating_sub(self.read_offset)
    }

    fn compact_if_useful(&mut self) {
        if self.read_offset == 0 {
            return;
        }
        let remaining = self.pending_len();
        if self.read_offset >= 4096 || self.read_offset >= remaining {
            self.buffer.copy_within(self.read_offset.., 0);
            self.buffer.truncate(remaining);
            self.read_offset = 0;
        }
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new(FrameLimits::default())
    }
}

/// Encodes one uncompressed packet body with a bounded VarInt frame length.
pub fn encode_frame(body: &[u8], max_frame_size: usize) -> Result<Vec<u8>, CodecError> {
    let effective_max = max_frame_size.min(MINECRAFT_MAX_FRAME_SIZE);
    if body.len() > effective_max {
        return Err(CodecError::FrameTooLong {
            length: body.len(),
            max: effective_max,
        });
    }
    let mut writer = CodecWriter::with_capacity(body.len().saturating_add(3));
    writer.write_length(body.len(), "frame length")?;
    writer.write_bytes(body);
    Ok(writer.into_inner())
}

/// Packet ID and uninterpreted payload borrowed from one completed frame body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawPacket<'a> {
    pub id: i32,
    pub payload: &'a [u8],
}

/// Separates a raw VarInt packet ID from the remaining frame bytes.
pub fn split_raw_packet(frame: &[u8]) -> Result<RawPacket<'_>, CodecError> {
    let mut reader = CodecReader::new(frame);
    let id = reader.read_var_int()?;
    Ok(RawPacket {
        id,
        payload: reader.read_remaining(),
    })
}
