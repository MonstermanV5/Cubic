use crate::{CodecError, CodecReader, CodecWriter};

const MAX_VAR_INT_BYTES: usize = 5;
const MAX_VAR_LONG_BYTES: usize = 10;

pub(crate) enum PartialVarInt {
    Complete { value: i32, bytes: usize },
    NeedMore,
    Malformed,
}

pub(crate) fn read_var_int(reader: &mut CodecReader<'_>) -> Result<i32, CodecError> {
    let mut result = 0_u32;
    for index in 0..MAX_VAR_INT_BYTES {
        let byte = reader.read_byte_with_context("VarInt")?;
        result |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(CodecError::MalformedVarInt)
}

pub(crate) fn read_var_long(reader: &mut CodecReader<'_>) -> Result<i64, CodecError> {
    let mut result = 0_u64;
    for index in 0..MAX_VAR_LONG_BYTES {
        let byte = reader.read_byte_with_context("VarLong")?;
        result |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(result as i64);
        }
    }
    Err(CodecError::MalformedVarLong)
}

pub(crate) fn write_var_int(writer: &mut CodecWriter, value: i32) {
    let mut remaining = value as u32;
    loop {
        if remaining & !0x7f == 0 {
            writer.write_u8(remaining as u8);
            return;
        }
        writer.write_u8(((remaining & 0x7f) | 0x80) as u8);
        remaining >>= 7;
    }
}

pub(crate) fn write_var_long(writer: &mut CodecWriter, value: i64) {
    let mut remaining = value as u64;
    loop {
        if remaining & !0x7f == 0 {
            writer.write_u8(remaining as u8);
            return;
        }
        writer.write_u8(((remaining & 0x7f) | 0x80) as u8);
        remaining >>= 7;
    }
}

pub(crate) fn decode_partial_var_int(input: &[u8]) -> PartialVarInt {
    let mut result = 0_u32;
    for (index, byte) in input.iter().copied().take(MAX_VAR_INT_BYTES).enumerate() {
        result |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return PartialVarInt::Complete {
                value: result as i32,
                bytes: index + 1,
            };
        }
    }
    if input.len() >= MAX_VAR_INT_BYTES {
        PartialVarInt::Malformed
    } else {
        PartialVarInt::NeedMore
    }
}
