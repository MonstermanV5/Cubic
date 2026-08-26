//! Safe, synchronous Minecraft Java binary protocol primitives.
//!
//! This crate contains no transport, packet schemas, packet IDs, compression,
//! encryption, authentication, or version-specific behavior.

pub mod nbt;

mod bitset;
mod error;
mod frame;
mod position;
mod reader;
mod uuid;
mod varint;
mod writer;

pub use bitset::{BitSet, BitSetLimits};
pub use error::{CodecError, LengthKind};
pub use frame::{
    DEFAULT_MAX_BUFFERED_BYTES, DEFAULT_MAX_FRAME_SIZE, FrameDecoder, FrameLimits,
    MINECRAFT_MAX_FRAME_SIZE, RawPacket, encode_frame, split_raw_packet,
};
pub use position::BlockPosition;
pub use reader::{CodecReader, StringLimits};
pub use uuid::ProtocolUuid;
pub use writer::CodecWriter;
