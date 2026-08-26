use crate::CodecWriter;

use super::{
    NamedNbtRoot, NbtCollectionKind, NbtCompound, NbtError, NbtLimits, NbtString, NbtTag,
    NbtTagType,
    string::{charge_budget, validate_encoded_length},
};

const COMPOUND_ENTRY_BUDGET: usize =
    size_of::<NbtString>() + size_of::<NbtTag>() + (4 * size_of::<usize>());

pub fn encode_named_root(root: &NamedNbtRoot, limits: NbtLimits) -> Result<Vec<u8>, NbtError> {
    let mut context = EncodeContext::new(limits);
    context.validate_root(&root.compound, Some(&root.name))?;
    let mut writer = CodecWriter::new();
    writer.write_u8(NbtTagType::Compound.id());
    root.name.encode(&mut writer, limits)?;
    encode_compound(&root.compound, &mut writer, limits)?;
    Ok(writer.into_inner())
}

pub fn encode_unnamed_network_root(
    root: &NbtCompound,
    limits: NbtLimits,
) -> Result<Vec<u8>, NbtError> {
    let mut context = EncodeContext::new(limits);
    context.validate_root(root, None)?;
    let mut writer = CodecWriter::new();
    writer.write_u8(NbtTagType::Compound.id());
    encode_compound(root, &mut writer, limits)?;
    Ok(writer.into_inner())
}

struct EncodeContext {
    limits: NbtLimits,
    total_tags: usize,
    resource_bytes: usize,
}

impl EncodeContext {
    const fn new(limits: NbtLimits) -> Self {
        Self {
            limits,
            total_tags: 0,
            resource_bytes: 0,
        }
    }

    fn validate_root(
        &mut self,
        root: &NbtCompound,
        name: Option<&NbtString>,
    ) -> Result<(), NbtError> {
        self.add_tag(0)?;
        if let Some(name) = name {
            self.validate_string(name)?;
        }
        self.validate_compound(root, 0)
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

    fn validate_tag(&mut self, tag: &NbtTag, depth: usize) -> Result<(), NbtError> {
        self.add_tag(depth)?;
        match tag {
            NbtTag::Byte(_)
            | NbtTag::Short(_)
            | NbtTag::Int(_)
            | NbtTag::Long(_)
            | NbtTag::Float(_)
            | NbtTag::Double(_) => Ok(()),
            NbtTag::ByteArray(values) => {
                self.validate_array(values.len(), NbtCollectionKind::ByteArray, 1)
            }
            NbtTag::String(value) => self.validate_string(value),
            NbtTag::List(list) => {
                list.validate()?;
                self.validate_collection_length(
                    NbtCollectionKind::List,
                    list.len(),
                    self.limits.max_list_elements(),
                )?;
                let allocation = checked_collection_bytes(
                    NbtCollectionKind::List,
                    list.len(),
                    size_of::<NbtTag>(),
                )?;
                charge_budget(&mut self.resource_bytes, allocation, self.limits)?;
                let child_depth = next_depth(depth, self.limits)?;
                for element in list.elements() {
                    self.validate_tag(element, child_depth)?;
                }
                Ok(())
            }
            NbtTag::Compound(compound) => self.validate_compound(compound, depth),
            NbtTag::IntArray(values) => {
                self.validate_array(values.len(), NbtCollectionKind::IntArray, size_of::<i32>())
            }
            NbtTag::LongArray(values) => {
                self.validate_array(values.len(), NbtCollectionKind::LongArray, size_of::<i64>())
            }
        }
    }

    fn validate_compound(&mut self, compound: &NbtCompound, depth: usize) -> Result<(), NbtError> {
        self.validate_collection_length(
            NbtCollectionKind::Compound,
            compound.len(),
            self.limits.max_compound_entries(),
        )?;
        let child_depth = next_depth(depth, self.limits)?;
        for (name, value) in compound.iter() {
            charge_budget(&mut self.resource_bytes, COMPOUND_ENTRY_BUDGET, self.limits)?;
            self.validate_string(name)?;
            self.validate_tag(value, child_depth)?;
        }
        Ok(())
    }

    fn validate_string(&mut self, value: &NbtString) -> Result<(), NbtError> {
        let length = value.encoded_len();
        validate_encoded_length(length, self.limits)?;
        let allocation =
            length
                .checked_mul(size_of::<u16>())
                .ok_or(NbtError::AllocationBudgetExceeded {
                    attempted: usize::MAX,
                    max: self.limits.max_total_allocated_bytes(),
                })?;
        charge_budget(&mut self.resource_bytes, allocation, self.limits)
    }

    fn validate_array(
        &mut self,
        length: usize,
        kind: NbtCollectionKind,
        element_size: usize,
    ) -> Result<(), NbtError> {
        self.validate_collection_length(kind, length, self.limits.max_array_elements())?;
        let bytes = checked_collection_bytes(kind, length, element_size)?;
        charge_budget(&mut self.resource_bytes, bytes, self.limits)
    }

    fn validate_collection_length(
        &self,
        kind: NbtCollectionKind,
        length: usize,
        configured_max: usize,
    ) -> Result<(), NbtError> {
        let max = configured_max.min(i32::MAX as usize);
        if length > max {
            Err(NbtError::CollectionTooLarge { kind, length, max })
        } else {
            Ok(())
        }
    }
}

fn encode_payload(
    tag: &NbtTag,
    writer: &mut CodecWriter,
    limits: NbtLimits,
) -> Result<(), NbtError> {
    match tag {
        NbtTag::Byte(value) => writer.write_i8(*value),
        NbtTag::Short(value) => writer.write_i16(*value),
        NbtTag::Int(value) => writer.write_i32(*value),
        NbtTag::Long(value) => writer.write_i64(*value),
        NbtTag::Float(value) => writer.write_f32(*value),
        NbtTag::Double(value) => writer.write_f64(*value),
        NbtTag::ByteArray(values) => {
            writer.write_i32(collection_length(
                values.len(),
                NbtCollectionKind::ByteArray,
            )?);
            writer.write_bytes(values);
        }
        NbtTag::String(value) => value.encode(writer, limits)?,
        NbtTag::List(list) => {
            writer.write_u8(list.element_type_id());
            writer.write_i32(collection_length(list.len(), NbtCollectionKind::List)?);
            for element in list.elements() {
                encode_payload(element, writer, limits)?;
            }
        }
        NbtTag::Compound(compound) => encode_compound(compound, writer, limits)?,
        NbtTag::IntArray(values) => {
            writer.write_i32(collection_length(
                values.len(),
                NbtCollectionKind::IntArray,
            )?);
            for value in values {
                writer.write_i32(*value);
            }
        }
        NbtTag::LongArray(values) => {
            writer.write_i32(collection_length(
                values.len(),
                NbtCollectionKind::LongArray,
            )?);
            for value in values {
                writer.write_i64(*value);
            }
        }
    }
    Ok(())
}

fn encode_compound(
    compound: &NbtCompound,
    writer: &mut CodecWriter,
    limits: NbtLimits,
) -> Result<(), NbtError> {
    for (name, value) in compound.iter() {
        writer.write_u8(value.tag_type().id());
        name.encode(writer, limits)?;
        encode_payload(value, writer, limits)?;
    }
    writer.write_u8(NbtTagType::End.id());
    Ok(())
}

fn collection_length(length: usize, kind: NbtCollectionKind) -> Result<i32, NbtError> {
    i32::try_from(length).map_err(|_| NbtError::CollectionTooLarge {
        kind,
        length,
        max: i32::MAX as usize,
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
