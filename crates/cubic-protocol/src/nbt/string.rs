use crate::{CodecReader, CodecWriter};

use super::{NbtError, NbtLimits};

/// Lossless Java string represented as arbitrary UTF-16 code units.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NbtString {
    utf16_units: Vec<u16>,
}

impl NbtString {
    #[must_use]
    pub fn from_utf16_units(utf16_units: Vec<u16>) -> Self {
        Self { utf16_units }
    }

    #[must_use]
    pub fn as_utf16_units(&self) -> &[u16] {
        &self.utf16_units
    }

    pub fn to_rust_string(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.utf16_units)
    }

    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.utf16_units)
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.utf16_units.iter().copied().map(unit_width).sum()
    }

    pub(crate) fn decode(
        reader: &mut CodecReader<'_>,
        limits: NbtLimits,
        budget: &mut usize,
    ) -> Result<Self, NbtError> {
        let encoded_length = usize::from(reader.read_u16()?);
        validate_encoded_length(encoded_length, limits)?;
        let bytes = reader.read_bytes(encoded_length, "NBT Modified UTF-8 bytes")?;
        let allocation = encoded_length.checked_mul(size_of::<u16>()).ok_or(
            NbtError::AllocationBudgetExceeded {
                attempted: usize::MAX,
                max: limits.max_total_allocated_bytes(),
            },
        )?;
        charge_budget(budget, allocation, limits)?;

        let mut units = Vec::new();
        units
            .try_reserve_exact(encoded_length)
            .map_err(|_| NbtError::AllocationFailed {
                context: "NBT string UTF-16 units",
                requested: encoded_length,
            })?;

        let mut offset = 0;
        while offset < bytes.len() {
            let first = *bytes
                .get(offset)
                .ok_or(NbtError::MalformedModifiedUtf8 { offset })?;
            let (unit, width) = match first {
                0x00..=0x7f => (u16::from(first), 1),
                0xc0..=0xdf => {
                    let second = continuation(bytes, offset, 1)?;
                    ((u16::from(first & 0x1f) << 6) | u16::from(second & 0x3f), 2)
                }
                0xe0..=0xef => {
                    let second = continuation(bytes, offset, 1)?;
                    let third = continuation(bytes, offset, 2)?;
                    (
                        (u16::from(first & 0x0f) << 12)
                            | (u16::from(second & 0x3f) << 6)
                            | u16::from(third & 0x3f),
                        3,
                    )
                }
                _ => return Err(NbtError::MalformedModifiedUtf8 { offset }),
            };
            units.push(unit);
            offset = offset
                .checked_add(width)
                .ok_or(NbtError::MalformedModifiedUtf8 { offset })?;
        }
        Ok(Self::from_utf16_units(units))
    }

    pub(crate) fn encode(
        &self,
        writer: &mut CodecWriter,
        limits: NbtLimits,
    ) -> Result<(), NbtError> {
        let encoded_length = self.encoded_len();
        validate_encoded_length(encoded_length, limits)?;
        let length = u16::try_from(encoded_length).map_err(|_| NbtError::StringTooLong {
            encoded_bytes: encoded_length,
            max: u16::MAX as usize,
        })?;
        writer.write_u16(length);
        for unit in &self.utf16_units {
            match *unit {
                0x0001..=0x007f => writer.write_u8(*unit as u8),
                0x0000..=0x07ff => {
                    writer.write_u8((0xc0 | (*unit >> 6)) as u8);
                    writer.write_u8((0x80 | (*unit & 0x3f)) as u8);
                }
                _ => {
                    writer.write_u8((0xe0 | (*unit >> 12)) as u8);
                    writer.write_u8((0x80 | ((*unit >> 6) & 0x3f)) as u8);
                    writer.write_u8((0x80 | (*unit & 0x3f)) as u8);
                }
            }
        }
        Ok(())
    }
}

impl From<&str> for NbtString {
    fn from(value: &str) -> Self {
        Self::from_utf16_units(value.encode_utf16().collect())
    }
}

impl From<String> for NbtString {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

fn continuation(bytes: &[u8], start: usize, relative: usize) -> Result<u8, NbtError> {
    let offset = start
        .checked_add(relative)
        .ok_or(NbtError::MalformedModifiedUtf8 { offset: start })?;
    let byte = bytes
        .get(offset)
        .copied()
        .ok_or(NbtError::MalformedModifiedUtf8 { offset })?;
    if byte & 0xc0 == 0x80 {
        Ok(byte)
    } else {
        Err(NbtError::MalformedModifiedUtf8 { offset })
    }
}

const fn unit_width(unit: u16) -> usize {
    match unit {
        0x0001..=0x007f => 1,
        0x0000..=0x07ff => 2,
        _ => 3,
    }
}

pub(crate) fn validate_encoded_length(
    encoded_length: usize,
    limits: NbtLimits,
) -> Result<(), NbtError> {
    let max = limits.max_string_encoded_bytes();
    if encoded_length > max {
        Err(NbtError::StringTooLong {
            encoded_bytes: encoded_length,
            max,
        })
    } else {
        Ok(())
    }
}

pub(crate) fn charge_budget(
    budget: &mut usize,
    amount: usize,
    limits: NbtLimits,
) -> Result<(), NbtError> {
    let attempted = budget
        .checked_add(amount)
        .ok_or(NbtError::AllocationBudgetExceeded {
            attempted: usize::MAX,
            max: limits.max_total_allocated_bytes(),
        })?;
    if attempted > limits.max_total_allocated_bytes() {
        return Err(NbtError::AllocationBudgetExceeded {
            attempted,
            max: limits.max_total_allocated_bytes(),
        });
    }
    *budget = attempted;
    Ok(())
}
