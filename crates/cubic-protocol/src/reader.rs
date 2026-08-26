use crate::{BitSet, BitSetLimits, BlockPosition, CodecError, LengthKind, ProtocolUuid, varint};

/// Explicit bounds for a Minecraft UTF-8 string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StringLimits {
    max_utf16_units: usize,
    max_encoded_bytes: usize,
}

impl StringLimits {
    /// Creates logical Java string and encoded-byte limits.
    #[must_use]
    pub const fn new(max_utf16_units: usize, max_encoded_bytes: usize) -> Self {
        Self {
            max_utf16_units,
            max_encoded_bytes,
        }
    }

    #[must_use]
    pub const fn max_utf16_units(self) -> usize {
        self.max_utf16_units
    }

    /// Returns the smaller of the configured byte cap and Minecraft's `n * 3` cap.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        match self.max_utf16_units.checked_mul(3) {
            Some(protocol_max) if protocol_max < self.max_encoded_bytes => protocol_max,
            Some(_) | None => self.max_encoded_bytes,
        }
    }
}

/// Cursor-based reader over borrowed, untrusted protocol bytes.
#[derive(Clone, Debug)]
pub struct CodecReader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> CodecReader<'a> {
    #[must_use]
    pub const fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.input.len() - self.position
    }

    /// Returns all unread bytes without copying and advances to the end.
    pub fn read_remaining(&mut self) -> &'a [u8] {
        let remaining = self.input.get(self.position..).unwrap_or_default();
        self.position = self.input.len();
        remaining
    }

    /// Reads exactly `length` bytes as a borrowed slice.
    pub fn read_bytes(
        &mut self,
        length: usize,
        context: &'static str,
    ) -> Result<&'a [u8], CodecError> {
        let remaining = self.remaining();
        let end = self
            .position
            .checked_add(length)
            .ok_or(CodecError::ValueOutOfRange {
                context: "reader byte length",
                value: length as i128,
                min: 0,
                max: usize::MAX as i128,
            })?;
        let bytes = self
            .input
            .get(self.position..end)
            .ok_or(CodecError::UnexpectedEnd {
                context,
                needed: length,
                remaining,
            })?;
        self.position = end;
        Ok(bytes)
    }

    pub(crate) fn read_byte_with_context(
        &mut self,
        context: &'static str,
    ) -> Result<u8, CodecError> {
        let bytes = self.read_bytes(1, context)?;
        bytes.first().copied().ok_or(CodecError::UnexpectedEnd {
            context,
            needed: 1,
            remaining: 0,
        })
    }

    pub fn read_i8(&mut self) -> Result<i8, CodecError> {
        Ok(i8::from_be_bytes([self.read_byte_with_context("i8")?]))
    }

    pub fn read_u8(&mut self) -> Result<u8, CodecError> {
        self.read_byte_with_context("u8")
    }

    pub fn read_i16(&mut self) -> Result<i16, CodecError> {
        Ok(i16::from_be_bytes(self.read_array("i16")?))
    }

    pub fn read_u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes(self.read_array("u16")?))
    }

    pub fn read_i32(&mut self) -> Result<i32, CodecError> {
        Ok(i32::from_be_bytes(self.read_array("i32")?))
    }

    pub fn read_u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.read_array("u32")?))
    }

    pub fn read_i64(&mut self) -> Result<i64, CodecError> {
        Ok(i64::from_be_bytes(self.read_array("i64")?))
    }

    pub fn read_u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.read_array("u64")?))
    }

    pub fn read_f32(&mut self) -> Result<f32, CodecError> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    pub fn read_f64(&mut self) -> Result<f64, CodecError> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    /// Decodes zero as false and every non-zero byte as true.
    pub fn read_bool(&mut self) -> Result<bool, CodecError> {
        Ok(self.read_u8()? != 0)
    }

    pub fn read_var_int(&mut self) -> Result<i32, CodecError> {
        varint::read_var_int(self)
    }

    pub fn read_var_long(&mut self) -> Result<i64, CodecError> {
        varint::read_var_long(self)
    }

    /// Reads and validates a bounded Minecraft UTF-8 string without copying.
    pub fn read_string(&mut self, limits: StringLimits) -> Result<&'a str, CodecError> {
        let length = self.read_length(LengthKind::String)?;
        let max_encoded_bytes = limits.max_encoded_bytes();
        if length > max_encoded_bytes {
            return Err(CodecError::EncodedStringTooLong {
                encoded_bytes: length,
                max_encoded_bytes,
            });
        }
        let bytes = self.read_bytes(length, "string bytes")?;
        let value = std::str::from_utf8(bytes).map_err(|error| CodecError::InvalidUtf8 {
            valid_up_to: error.valid_up_to(),
        })?;
        let utf16_units = value.encode_utf16().count();
        if utf16_units > limits.max_utf16_units() {
            return Err(CodecError::StringTooLong {
                utf16_units,
                max_utf16_units: limits.max_utf16_units(),
            });
        }
        Ok(value)
    }

    /// Reads a bounded VarInt-length-prefixed byte array without copying.
    pub fn read_byte_array(&mut self, max_length: usize) -> Result<&'a [u8], CodecError> {
        let length = self.read_length(LengthKind::ByteArray)?;
        if length > max_length {
            return Err(CodecError::ByteArrayTooLong {
                length,
                max: max_length,
            });
        }
        self.read_bytes(length, "byte array")
    }

    pub fn read_uuid(&mut self) -> Result<ProtocolUuid, CodecError> {
        Ok(ProtocolUuid::from_bytes(self.read_array("UUID")?))
    }

    pub fn read_block_position(&mut self) -> Result<BlockPosition, CodecError> {
        Ok(BlockPosition::from_packed(self.read_u64()?))
    }

    pub fn read_bitset(&mut self, limits: BitSetLimits) -> Result<BitSet, CodecError> {
        BitSet::decode(self, limits)
    }

    pub(crate) fn read_length(&mut self, kind: LengthKind) -> Result<usize, CodecError> {
        let value = match self.read_var_int() {
            Err(CodecError::MalformedVarInt) => {
                return Err(CodecError::MalformedLengthPrefix { kind });
            }
            result => result?,
        };
        if value < 0 {
            return Err(CodecError::NegativeLength { kind, value });
        }
        usize::try_from(value).map_err(|_| CodecError::ValueOutOfRange {
            context: "length prefix",
            value: i128::from(value),
            min: 0,
            max: usize::MAX as i128,
        })
    }

    fn read_array<const N: usize>(&mut self, context: &'static str) -> Result<[u8; N], CodecError> {
        let bytes = self.read_bytes(N, context)?;
        <[u8; N]>::try_from(bytes).map_err(|_| CodecError::UnexpectedEnd {
            context,
            needed: N,
            remaining: bytes.len(),
        })
    }
}
