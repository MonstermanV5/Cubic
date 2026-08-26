use crate::CodecError;
use thiserror::Error;

use super::NbtTagType;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NbtCollectionKind {
    ByteArray,
    IntArray,
    LongArray,
    List,
    Compound,
}

impl std::fmt::Display for NbtCollectionKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ByteArray => formatter.write_str("TAG_Byte_Array"),
            Self::IntArray => formatter.write_str("TAG_Int_Array"),
            Self::LongArray => formatter.write_str("TAG_Long_Array"),
            Self::List => formatter.write_str("TAG_List"),
            Self::Compound => formatter.write_str("TAG_Compound"),
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NbtError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("invalid NBT tag ID {id}")]
    InvalidTagId { id: u8 },
    #[error("unexpected TAG_End in {context}")]
    UnexpectedEndTag { context: &'static str },
    #[error("NBT root must be TAG_Compound, found tag ID {found}")]
    InvalidRootType { found: u8 },
    #[error("malformed Modified UTF-8 at byte offset {offset}")]
    MalformedModifiedUtf8 { offset: usize },
    #[error("Modified UTF-8 string has {encoded_bytes} bytes, exceeding limit {max}")]
    StringTooLong { encoded_bytes: usize, max: usize },
    #[error("negative {kind} length {value}")]
    NegativeCollectionLength { kind: NbtCollectionKind, value: i32 },
    #[error("{kind} has {length} elements, exceeding limit {max}")]
    CollectionTooLarge {
        kind: NbtCollectionKind,
        length: usize,
        max: usize,
    },
    #[error("{kind} length arithmetic overflow for {length} elements of {element_size} bytes")]
    CollectionSizeOverflow {
        kind: NbtCollectionKind,
        length: usize,
        element_size: usize,
    },
    #[error("NBT nesting depth {depth} exceeds limit {max}")]
    DepthLimitExceeded { depth: usize, max: usize },
    #[error("NBT tag count {count} exceeds limit {max}")]
    TotalTagLimitExceeded { count: usize, max: usize },
    #[error("NBT resource budget would reach {attempted} bytes, exceeding limit {max}")]
    AllocationBudgetExceeded { attempted: usize, max: usize },
    #[error("could not reserve {requested} elements for {context}")]
    AllocationFailed {
        context: &'static str,
        requested: usize,
    },
    #[error("heterogeneous TAG_List element {index}: expected {expected:?}, found {found:?}")]
    HeterogeneousList {
        expected: NbtTagType,
        found: NbtTagType,
        index: usize,
    },
    #[error("non-empty TAG_List cannot declare TAG_End elements")]
    EndListWithElements,
    #[error("trailing bytes after complete NBT document: {remaining}")]
    TrailingData { remaining: usize },
}
