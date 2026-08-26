use crate::{BitSet, BitSetLimits, BlockPosition, CodecError, ProtocolUuid, StringLimits, varint};

/// Deterministic writer for Minecraft binary protocol values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodecWriter {
    output: Vec<u8>,
}

impl CodecWriter {
    #[must_use]
    pub const fn new() -> Self {
        Self { output: Vec::new() }
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            output: Vec::with_capacity(capacity),
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.output
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.output.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.output.is_empty()
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.output
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.output.extend_from_slice(bytes);
    }

    pub fn write_i8(&mut self, value: i8) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_u8(&mut self, value: u8) {
        self.output.push(value);
    }

    pub fn write_i16(&mut self, value: i16) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_i32(&mut self, value: i32) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_i64(&mut self, value: i64) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_be_bytes());
    }

    pub fn write_f32(&mut self, value: f32) {
        self.write_u32(value.to_bits());
    }

    pub fn write_f64(&mut self, value: f64) {
        self.write_u64(value.to_bits());
    }

    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    pub fn write_var_int(&mut self, value: i32) {
        varint::write_var_int(self, value);
    }

    pub fn write_var_long(&mut self, value: i64) {
        varint::write_var_long(self, value);
    }

    pub fn write_string(&mut self, value: &str, limits: StringLimits) -> Result<(), CodecError> {
        let utf16_units = value.encode_utf16().count();
        if utf16_units > limits.max_utf16_units() {
            return Err(CodecError::StringTooLong {
                utf16_units,
                max_utf16_units: limits.max_utf16_units(),
            });
        }
        let encoded_bytes = value.len();
        let max_encoded_bytes = limits.max_encoded_bytes();
        if encoded_bytes > max_encoded_bytes {
            return Err(CodecError::EncodedStringTooLong {
                encoded_bytes,
                max_encoded_bytes,
            });
        }
        self.write_length(encoded_bytes, "string length")?;
        self.write_bytes(value.as_bytes());
        Ok(())
    }

    pub fn write_byte_array(&mut self, value: &[u8], max_length: usize) -> Result<(), CodecError> {
        if value.len() > max_length {
            return Err(CodecError::ByteArrayTooLong {
                length: value.len(),
                max: max_length,
            });
        }
        self.write_length(value.len(), "byte array length")?;
        self.write_bytes(value);
        Ok(())
    }

    pub fn write_uuid(&mut self, value: ProtocolUuid) {
        self.write_bytes(&value.to_bytes());
    }

    pub fn write_block_position(&mut self, x: i32, y: i32, z: i32) -> Result<(), CodecError> {
        let position = BlockPosition::new(x, y, z)?;
        self.write_u64(position.to_packed());
        Ok(())
    }

    pub fn write_bitset(&mut self, value: &BitSet, limits: BitSetLimits) -> Result<(), CodecError> {
        value.encode(self, limits)
    }

    pub(crate) fn write_length(
        &mut self,
        length: usize,
        context: &'static str,
    ) -> Result<(), CodecError> {
        let value = i32::try_from(length).map_err(|_| CodecError::ValueOutOfRange {
            context,
            value: length as i128,
            min: 0,
            max: i128::from(i32::MAX),
        })?;
        self.write_var_int(value);
        Ok(())
    }
}
