use std::collections::BTreeMap;

use super::{NbtError, NbtString};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum NbtTagType {
    End = 0,
    Byte = 1,
    Short = 2,
    Int = 3,
    Long = 4,
    Float = 5,
    Double = 6,
    ByteArray = 7,
    String = 8,
    List = 9,
    Compound = 10,
    IntArray = 11,
    LongArray = 12,
}

impl NbtTagType {
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::End),
            1 => Some(Self::Byte),
            2 => Some(Self::Short),
            3 => Some(Self::Int),
            4 => Some(Self::Long),
            5 => Some(Self::Float),
            6 => Some(Self::Double),
            7 => Some(Self::ByteArray),
            8 => Some(Self::String),
            9 => Some(Self::List),
            10 => Some(Self::Compound),
            11 => Some(Self::IntArray),
            12 => Some(Self::LongArray),
            _ => None,
        }
    }

    pub const fn id(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NbtList {
    element_type_id: u8,
    elements: Vec<NbtTag>,
}

impl NbtList {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            element_type_id: NbtTagType::End.id(),
            elements: Vec::new(),
        }
    }

    pub fn new(element_type: NbtTagType, elements: Vec<NbtTag>) -> Result<Self, NbtError> {
        validate_list(element_type.id(), &elements)?;
        Ok(Self {
            element_type_id: element_type.id(),
            elements,
        })
    }

    #[must_use]
    pub fn empty_with_type_id(element_type_id: u8) -> Self {
        Self {
            element_type_id,
            elements: Vec::new(),
        }
    }

    #[must_use]
    pub const fn element_type_id(&self) -> u8 {
        self.element_type_id
    }

    #[must_use]
    pub const fn element_type(&self) -> Option<NbtTagType> {
        NbtTagType::from_id(self.element_type_id)
    }

    #[must_use]
    pub fn elements(&self) -> &[NbtTag] {
        &self.elements
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub(crate) fn validate(&self) -> Result<(), NbtError> {
        validate_list(self.element_type_id, &self.elements)
    }
}

impl Default for NbtList {
    fn default() -> Self {
        Self::empty()
    }
}

fn validate_list(element_type_id: u8, elements: &[NbtTag]) -> Result<(), NbtError> {
    if elements.is_empty() {
        return Ok(());
    }
    let expected = NbtTagType::from_id(element_type_id).ok_or(NbtError::InvalidTagId {
        id: element_type_id,
    })?;
    if expected == NbtTagType::End {
        return Err(NbtError::EndListWithElements);
    }
    for (index, element) in elements.iter().enumerate() {
        let found = element.tag_type();
        if found != expected {
            return Err(NbtError::HeterogeneousList {
                expected,
                found,
                index,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NbtCompound {
    entries: BTreeMap<NbtString, NbtTag>,
}

impl NbtCompound {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, name: NbtString, value: NbtTag) -> Option<NbtTag> {
        self.entries.insert(name, value)
    }

    #[must_use]
    pub fn get(&self, name: &NbtString) -> Option<&NbtTag> {
        self.entries.get(name)
    }

    #[must_use]
    pub fn get_str(&self, name: &str) -> Option<&NbtTag> {
        self.entries.iter().find_map(|(candidate, value)| {
            candidate
                .as_utf16_units()
                .iter()
                .copied()
                .eq(name.encode_utf16())
                .then_some(value)
        })
    }

    #[must_use]
    pub fn get_int(&self, name: &str) -> Option<i32> {
        match self.get_str(name) {
            Some(NbtTag::Int(value)) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn get_string(&self, name: &str) -> Option<&NbtString> {
        match self.get_str(name) {
            Some(NbtTag::String(value)) => Some(value),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&NbtString, &NbtTag)> {
        self.entries.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug)]
pub enum NbtTag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(NbtString),
    List(NbtList),
    Compound(NbtCompound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl NbtTag {
    #[must_use]
    pub const fn tag_type(&self) -> NbtTagType {
        match self {
            Self::Byte(_) => NbtTagType::Byte,
            Self::Short(_) => NbtTagType::Short,
            Self::Int(_) => NbtTagType::Int,
            Self::Long(_) => NbtTagType::Long,
            Self::Float(_) => NbtTagType::Float,
            Self::Double(_) => NbtTagType::Double,
            Self::ByteArray(_) => NbtTagType::ByteArray,
            Self::String(_) => NbtTagType::String,
            Self::List(_) => NbtTagType::List,
            Self::Compound(_) => NbtTagType::Compound,
            Self::IntArray(_) => NbtTagType::IntArray,
            Self::LongArray(_) => NbtTagType::LongArray,
        }
    }
}

impl PartialEq for NbtTag {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Byte(left), Self::Byte(right)) => left == right,
            (Self::Short(left), Self::Short(right)) => left == right,
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Long(left), Self::Long(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => left.to_bits() == right.to_bits(),
            (Self::Double(left), Self::Double(right)) => left.to_bits() == right.to_bits(),
            (Self::ByteArray(left), Self::ByteArray(right)) => left == right,
            (Self::String(left), Self::String(right)) => left == right,
            (Self::List(left), Self::List(right)) => left == right,
            (Self::Compound(left), Self::Compound(right)) => left == right,
            (Self::IntArray(left), Self::IntArray(right)) => left == right,
            (Self::LongArray(left), Self::LongArray(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for NbtTag {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedNbtRoot {
    pub name: NbtString,
    pub compound: NbtCompound,
}
