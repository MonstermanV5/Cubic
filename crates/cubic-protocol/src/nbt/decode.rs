use crate::{CodecError, CodecReader};

use super::{
    NamedNbtRoot, NbtCollectionKind, NbtCompound, NbtError, NbtLimits, NbtList, NbtString, NbtTag,
    NbtTagType, string::charge_budget,
};

const COMPOUND_ENTRY_BUDGET: usize =
    size_of::<NbtString>() + size_of::<NbtTag>() + (4 * size_of::<usize>());

pub fn decode_named_root(
    reader: &mut CodecReader<'_>,
    limits: NbtLimits,
) -> Result<NamedNbtRoot, NbtError> {
    require_compound_root(reader)?;
    let mut context = DecodeContext::new(limits);
    context.add_tag(0)?;
    let name = NbtString::decode(reader, limits, &mut context.allocated_bytes)?;
    let compound = context.decode_compound(reader, 0)?;
    Ok(NamedNbtRoot { name, compound })
}

pub fn decode_unnamed_network_root(
    reader: &mut CodecReader<'_>,
    limits: NbtLimits,
) -> Result<NbtCompound, NbtError> {
    require_compound_root(reader)?;
    let mut context = DecodeContext::new(limits);
    context.add_tag(0)?;
    context.decode_compound(reader, 0)
}

/// Decodes the unnamed network form of any non-End NBT tag.
///
/// Modern text components use this generic root form rather than requiring a
/// compound root. The same resource limits apply to the complete value.
pub fn decode_unnamed_network_tag(
    reader: &mut CodecReader<'_>,
    limits: NbtLimits,
) -> Result<NbtTag, NbtError> {
    let id = read_type_id(reader, "NBT root type")?;
    if id == NbtTagType::End.id() {
        return Err(NbtError::UnexpectedEndTag {
            context: "NBT root",
        });
    }
    let tag_type = NbtTagType::from_id(id).ok_or(NbtError::InvalidTagId { id })?;
    let mut context = DecodeContext::new(limits);
    context.decode_payload(reader, tag_type, 0)
}

pub fn decode_named_root_complete(
    input: &[u8],
    limits: NbtLimits,
) -> Result<NamedNbtRoot, NbtError> {
    let mut reader = CodecReader::new(input);
    let root = decode_named_root(&mut reader, limits)?;
    reject_trailing(&reader)?;
    Ok(root)
}

pub fn decode_unnamed_network_root_complete(
    input: &[u8],
    limits: NbtLimits,
) -> Result<NbtCompound, NbtError> {
    let mut reader = CodecReader::new(input);
    let root = decode_unnamed_network_root(&mut reader, limits)?;
    reject_trailing(&reader)?;
    Ok(root)
}

pub fn decode_unnamed_network_tag_complete(
    input: &[u8],
    limits: NbtLimits,
) -> Result<NbtTag, NbtError> {
    let mut reader = CodecReader::new(input);
    let root = decode_unnamed_network_tag(&mut reader, limits)?;
    reject_trailing(&reader)?;
    Ok(root)
}

fn require_compound_root(reader: &mut CodecReader<'_>) -> Result<(), NbtError> {
    let id = read_type_id(reader, "NBT root type")?;
    if id == NbtTagType::End.id() {
        return Err(NbtError::UnexpectedEndTag {
            context: "NBT root",
        });
    }
    if id != NbtTagType::Compound.id() {
        return Err(NbtError::InvalidRootType { found: id });
    }
    Ok(())
}

fn reject_trailing(reader: &CodecReader<'_>) -> Result<(), NbtError> {
    if reader.remaining() == 0 {
        Ok(())
    } else {
        Err(NbtError::TrailingData {
            remaining: reader.remaining(),
        })
    }
}

struct DecodeContext {
    limits: NbtLimits,
    total_tags: usize,
    allocated_bytes: usize,
}

impl DecodeContext {
    const fn new(limits: NbtLimits) -> Self {
        Self {
            limits,
            total_tags: 0,
            allocated_bytes: 0,
        }
    }

    fn add_tag(&mut self, depth: usize) -> Result<(), NbtError> {
        if depth > self.limits.max_depth() {
            return Err(NbtError::DepthLimitExceeded {
                depth,
                max: self.limits.max_depth(),
            });
        }
        let count = self
            .total_tags
            .checked_add(1)
            .ok_or(NbtError::TotalTagLimitExceeded {
                count: usize::MAX,
                max: self.limits.max_total_tags(),
            })?;
        if count > self.limits.max_total_tags() {
            return Err(NbtError::TotalTagLimitExceeded {
                count,
                max: self.limits.max_total_tags(),
            });
        }
        self.total_tags = count;
        Ok(())
    }

    fn decode_payload(
        &mut self,
        reader: &mut CodecReader<'_>,
        tag_type: NbtTagType,
        depth: usize,
    ) -> Result<NbtTag, NbtError> {
        self.add_tag(depth)?;
        match tag_type {
            NbtTagType::End => Err(NbtError::UnexpectedEndTag {
                context: "NBT payload",
            }),
            NbtTagType::Byte => Ok(NbtTag::Byte(reader.read_i8()?)),
            NbtTagType::Short => Ok(NbtTag::Short(reader.read_i16()?)),
            NbtTagType::Int => Ok(NbtTag::Int(reader.read_i32()?)),
            NbtTagType::Long => Ok(NbtTag::Long(reader.read_i64()?)),
            NbtTagType::Float => Ok(NbtTag::Float(reader.read_f32()?)),
            NbtTagType::Double => Ok(NbtTag::Double(reader.read_f64()?)),
            NbtTagType::ByteArray => self.decode_byte_array(reader),
            NbtTagType::String => Ok(NbtTag::String(NbtString::decode(
                reader,
                self.limits,
                &mut self.allocated_bytes,
            )?)),
            NbtTagType::List => self.decode_list(reader, depth),
            NbtTagType::Compound => Ok(NbtTag::Compound(self.decode_compound(reader, depth)?)),
            NbtTagType::IntArray => self.decode_int_array(reader),
            NbtTagType::LongArray => self.decode_long_array(reader),
        }
    }

    fn decode_byte_array(&mut self, reader: &mut CodecReader<'_>) -> Result<NbtTag, NbtError> {
        let length = self.read_collection_length(reader, NbtCollectionKind::ByteArray)?;
        self.check_array_length(length, NbtCollectionKind::ByteArray, 1)?;
        let bytes = reader.read_bytes(length, "TAG_Byte_Array payload")?;
        charge_budget(&mut self.allocated_bytes, length, self.limits)?;
        let mut output = Vec::new();
        reserve(&mut output, length, "TAG_Byte_Array elements")?;
        output.extend_from_slice(bytes);
        Ok(NbtTag::ByteArray(output))
    }

    fn decode_int_array(&mut self, reader: &mut CodecReader<'_>) -> Result<NbtTag, NbtError> {
        let length = self.read_collection_length(reader, NbtCollectionKind::IntArray)?;
        let byte_length =
            self.check_array_length(length, NbtCollectionKind::IntArray, size_of::<i32>())?;
        let bytes = reader.read_bytes(byte_length, "TAG_Int_Array payload")?;
        charge_budget(&mut self.allocated_bytes, byte_length, self.limits)?;
        let mut output = Vec::new();
        reserve(&mut output, length, "TAG_Int_Array elements")?;
        let mut values = CodecReader::new(bytes);
        for _ in 0..length {
            output.push(values.read_i32()?);
        }
        Ok(NbtTag::IntArray(output))
    }

    fn decode_long_array(&mut self, reader: &mut CodecReader<'_>) -> Result<NbtTag, NbtError> {
        let length = self.read_collection_length(reader, NbtCollectionKind::LongArray)?;
        let byte_length =
            self.check_array_length(length, NbtCollectionKind::LongArray, size_of::<i64>())?;
        let bytes = reader.read_bytes(byte_length, "TAG_Long_Array payload")?;
        charge_budget(&mut self.allocated_bytes, byte_length, self.limits)?;
        let mut output = Vec::new();
        reserve(&mut output, length, "TAG_Long_Array elements")?;
        let mut values = CodecReader::new(bytes);
        for _ in 0..length {
            output.push(values.read_i64()?);
        }
        Ok(NbtTag::LongArray(output))
    }

    fn decode_list(
        &mut self,
        reader: &mut CodecReader<'_>,
        depth: usize,
    ) -> Result<NbtTag, NbtError> {
        let element_type_id = read_type_id(reader, "TAG_List element type")?;
        let signed_length = reader.read_i32()?;
        if signed_length <= 0 {
            return Ok(NbtTag::List(NbtList::empty_with_type_id(element_type_id)));
        }
        let length =
            usize::try_from(signed_length).map_err(|_| NbtError::CollectionSizeOverflow {
                kind: NbtCollectionKind::List,
                length: usize::MAX,
                element_size: size_of::<NbtTag>(),
            })?;
        self.check_collection_limit(
            NbtCollectionKind::List,
            length,
            self.limits.max_list_elements(),
        )?;
        let element_type = NbtTagType::from_id(element_type_id).ok_or(NbtError::InvalidTagId {
            id: element_type_id,
        })?;
        if element_type == NbtTagType::End {
            return Err(NbtError::EndListWithElements);
        }
        let allocation =
            checked_collection_bytes(NbtCollectionKind::List, length, size_of::<NbtTag>())?;
        charge_budget(&mut self.allocated_bytes, allocation, self.limits)?;
        let mut elements = Vec::new();
        reserve(&mut elements, length, "TAG_List elements")?;
        let child_depth = next_depth(depth, self.limits)?;
        for _ in 0..length {
            elements.push(self.decode_payload(reader, element_type, child_depth)?);
        }
        Ok(NbtTag::List(NbtList::new(element_type, elements)?))
    }

    fn decode_compound(
        &mut self,
        reader: &mut CodecReader<'_>,
        depth: usize,
    ) -> Result<NbtCompound, NbtError> {
        let mut compound = NbtCompound::new();
        let mut wire_entries = 0_usize;
        loop {
            let type_id = read_type_id(reader, "TAG_Compound child type")?;
            if type_id == NbtTagType::End.id() {
                return Ok(compound);
            }
            wire_entries = wire_entries
                .checked_add(1)
                .ok_or(NbtError::CollectionTooLarge {
                    kind: NbtCollectionKind::Compound,
                    length: usize::MAX,
                    max: self.limits.max_compound_entries(),
                })?;
            self.check_collection_limit(
                NbtCollectionKind::Compound,
                wire_entries,
                self.limits.max_compound_entries(),
            )?;
            let tag_type =
                NbtTagType::from_id(type_id).ok_or(NbtError::InvalidTagId { id: type_id })?;
            charge_budget(
                &mut self.allocated_bytes,
                COMPOUND_ENTRY_BUDGET,
                self.limits,
            )?;
            let name = NbtString::decode(reader, self.limits, &mut self.allocated_bytes)?;
            let child_depth = next_depth(depth, self.limits)?;
            let value = self.decode_payload(reader, tag_type, child_depth)?;
            compound.insert(name, value);
        }
    }

    fn read_collection_length(
        &self,
        reader: &mut CodecReader<'_>,
        kind: NbtCollectionKind,
    ) -> Result<usize, NbtError> {
        let value = reader.read_i32()?;
        if value < 0 {
            return Err(NbtError::NegativeCollectionLength { kind, value });
        }
        usize::try_from(value).map_err(|_| NbtError::CollectionSizeOverflow {
            kind,
            length: usize::MAX,
            element_size: 1,
        })
    }

    fn check_array_length(
        &self,
        length: usize,
        kind: NbtCollectionKind,
        element_size: usize,
    ) -> Result<usize, NbtError> {
        self.check_collection_limit(kind, length, self.limits.max_array_elements())?;
        checked_collection_bytes(kind, length, element_size)
    }

    fn check_collection_limit(
        &self,
        kind: NbtCollectionKind,
        length: usize,
        max: usize,
    ) -> Result<(), NbtError> {
        if length > max {
            Err(NbtError::CollectionTooLarge { kind, length, max })
        } else {
            Ok(())
        }
    }
}

fn read_type_id(reader: &mut CodecReader<'_>, context: &'static str) -> Result<u8, NbtError> {
    let bytes = reader.read_bytes(1, context)?;
    bytes.first().copied().ok_or({
        NbtError::Codec(CodecError::UnexpectedEnd {
            context,
            needed: 1,
            remaining: 0,
        })
    })
}

fn next_depth(depth: usize, limits: NbtLimits) -> Result<usize, NbtError> {
    depth.checked_add(1).ok_or(NbtError::DepthLimitExceeded {
        depth: usize::MAX,
        max: limits.max_depth(),
    })
}

fn checked_collection_bytes(
    kind: NbtCollectionKind,
    length: usize,
    element_size: usize,
) -> Result<usize, NbtError> {
    length
        .checked_mul(element_size)
        .ok_or(NbtError::CollectionSizeOverflow {
            kind,
            length,
            element_size,
        })
}

fn reserve<T>(output: &mut Vec<T>, length: usize, context: &'static str) -> Result<(), NbtError> {
    output
        .try_reserve_exact(length)
        .map_err(|_| NbtError::AllocationFailed {
            context,
            requested: length,
        })
}
