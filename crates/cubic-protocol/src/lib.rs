//! Safe, synchronous Minecraft Java binary protocol primitives.
//!
//! This crate contains no transport, compression, encryption, authentication,
//! or general generated packet schemas. The manually authored protocol-775
//! bootstrap is a narrow Phase 7 exception that Phase 12 will replace or absorb.

pub mod bootstrap;
pub mod handshake;
pub mod nbt;
pub mod packet_schema;
pub mod status;

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
