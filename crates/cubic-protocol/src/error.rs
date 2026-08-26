use thiserror::Error;

/// Identifies a length prefix without tying the codec to a packet schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LengthKind {
    String,
    ByteArray,
    BitSet,
    Frame,
}

impl std::fmt::Display for LengthKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::String => formatter.write_str("string"),
            Self::ByteArray => formatter.write_str("byte array"),
            Self::BitSet => formatter.write_str("BitSet"),
            Self::Frame => formatter.write_str("frame"),
        }
    }
}

/// Structured failure returned while encoding or decoding protocol data.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CodecError {
    #[error(
        "unexpected end while reading {context}: needed {needed} bytes, only {remaining} remain"
    )]
    UnexpectedEnd {
        context: &'static str,
        needed: usize,
        remaining: usize,
    },
    #[error("malformed VarInt: encoded value exceeds 5 bytes")]
    MalformedVarInt,
    #[error("malformed VarLong: encoded value exceeds 10 bytes")]
    MalformedVarLong,
    #[error("malformed {kind} length prefix")]
    MalformedLengthPrefix { kind: LengthKind },
    #[error("negative {kind} length {value}")]
    NegativeLength { kind: LengthKind, value: i32 },
    #[error("{context} value {value} is outside permitted range {min}..={max}")]
    ValueOutOfRange {
        context: &'static str,
        value: i128,
        min: i128,
        max: i128,
    },
    #[error("invalid UTF-8 string at byte offset {valid_up_to}")]
    InvalidUtf8 { valid_up_to: usize },
    #[error("string has {utf16_units} UTF-16 code units, exceeding limit {max_utf16_units}")]
    StringTooLong {
        utf16_units: usize,
        max_utf16_units: usize,
    },
    #[error("encoded string has {encoded_bytes} bytes, exceeding limit {max_encoded_bytes}")]
    EncodedStringTooLong {
        encoded_bytes: usize,
        max_encoded_bytes: usize,
    },
    #[error("byte array has {length} bytes, exceeding limit {max}")]
    ByteArrayTooLong { length: usize, max: usize },
    #[error("BitSet has {words} words, exceeding limit {max_words}")]
    BitSetTooManyWords { words: usize, max_words: usize },
    #[error("BitSet contains bit {bit}, outside limit of {max_bits} bits")]
    BitSetBitOutOfRange { bit: usize, max_bits: usize },
    #[error("frame has {length} bytes, exceeding limit {max}")]
    FrameTooLong { length: usize, max: usize },
    #[error("frame decoder would buffer {buffered} bytes, exceeding limit {max}")]
    FrameBufferTooLong { buffered: usize, max: usize },
    #[error("block position {axis} coordinate {value} is outside range {min}..={max}")]
    InvalidBlockPosition {
        axis: &'static str,
        value: i32,
        min: i32,
        max: i32,
    },
    #[error("could not reserve {requested} elements for {context}")]
    AllocationFailed {
        context: &'static str,
        requested: usize,
    },
}
