use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{CodecError, CodecReader, CodecWriter, StringLimits, encode_frame, split_raw_packet};

pub const HANDSHAKE_PACKET_ID: i32 = 0;
pub const STATUS_REQUEST_PACKET_ID: i32 = 0;
pub const STATUS_RESPONSE_PACKET_ID: i32 = 0;
pub const PING_PACKET_ID: i32 = 1;
pub const PONG_PACKET_ID: i32 = 1;
pub const STATUS_NEXT_STATE: i32 = 1;
pub const STATUS_PROBE_PROTOCOL_VERSION: i32 = -1;
pub const MAX_HANDSHAKE_HOST_UTF16_UNITS: usize = 255;
pub const MAX_STATUS_JSON_UTF16_UNITS: usize = 32_767;
pub const MAX_STATUS_JSON_ENCODED_BYTES: usize = MAX_STATUS_JSON_UTF16_UNITS * 3;
pub const MAX_STATUS_FRAME_SIZE: usize = 128 * 1024;
pub const MAX_STATUS_SAMPLE_ENTRIES: usize = 100;
pub const MAX_STATUS_FAVICON_BYTES: usize = 32 * 1024;
pub const MAX_STATUS_TEXT_FIELD_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusHandshake<'a> {
    pub protocol_version: i32,
    pub server_address: &'a str,
    pub server_port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusVersion {
    pub name: String,
    pub protocol: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusPlayerSample {
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusPlayers {
    pub max: i32,
    pub online: i32,
    pub sample: Vec<StatusPlayerSample>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusResponse {
    pub version: StatusVersion,
    pub players: StatusPlayers,
    pub description: Value,
    pub favicon: Option<String>,
    pub additional_fields: BTreeMap<String, Value>,
    pub raw_json: String,
}

impl StatusResponse {
    #[must_use]
    pub fn motd_preview(&self) -> Option<&str> {
        match &self.description {
            Value::String(value) => Some(value),
            Value::Object(object) => object.get("text").and_then(Value::as_str),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusJsonLimits {
    max_sample_entries: usize,
    max_favicon_bytes: usize,
    max_text_field_bytes: usize,
}

impl StatusJsonLimits {
    #[must_use]
    pub const fn new(
        max_sample_entries: usize,
        max_favicon_bytes: usize,
        max_text_field_bytes: usize,
    ) -> Self {
        Self {
            max_sample_entries,
            max_favicon_bytes,
            max_text_field_bytes,
        }
    }
}

impl Default for StatusJsonLimits {
    fn default() -> Self {
        Self::new(
            MAX_STATUS_SAMPLE_ENTRIES,
            MAX_STATUS_FAVICON_BYTES,
            MAX_STATUS_TEXT_FIELD_BYTES,
        )
    }
}

#[derive(Debug, Error)]
pub enum StatusProtocolError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("unexpected Status packet ID {actual}; expected {expected}")]
    UnexpectedPacketId { expected: i32, actual: i32 },
    #[error("Status packet has {remaining} unexpected trailing bytes")]
    TrailingPacketData { remaining: usize },
    #[error("malformed Status JSON at line {line}, column {column}")]
    MalformedJson { line: usize, column: usize },
    #[error("Status JSON is missing a required field or contains an invalid field type")]
    InvalidStatusData,
    #[error("Status player counts must be non-negative (online {online}, max {max})")]
    InvalidPlayerCounts { online: i32, max: i32 },
    #[error("Status player sample has {count} entries, exceeding limit {max}")]
    PlayerSampleTooLarge { count: usize, max: usize },
    #[error("Status {field} has {bytes} bytes, exceeding limit {max}")]
    TextFieldTooLarge {
        field: &'static str,
        bytes: usize,
        max: usize,
    },
    #[error("Pong nonce {actual} does not match sent nonce {expected}")]
    PongMismatch { expected: i64, actual: i64 },
}

#[derive(Deserialize)]
struct WireStatusResponse {
    version: WireStatusVersion,
    players: WireStatusPlayers,
    description: Value,
    #[serde(default)]
    favicon: Option<String>,
    #[serde(flatten)]
    additional_fields: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct WireStatusVersion {
    name: String,
    protocol: i32,
}

#[derive(Deserialize)]
struct WireStatusPlayers {
    max: i32,
    online: i32,
    #[serde(default)]
    sample: Vec<WireStatusPlayerSample>,
}

#[derive(Deserialize)]
struct WireStatusPlayerSample {
    name: String,
    id: String,
}

pub fn encode_status_handshake(handshake: &StatusHandshake<'_>) -> Result<Vec<u8>, CodecError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(HANDSHAKE_PACKET_ID);
    writer.write_var_int(handshake.protocol_version);
    writer.write_string(
        handshake.server_address,
        StringLimits::new(
            MAX_HANDSHAKE_HOST_UTF16_UNITS,
            MAX_HANDSHAKE_HOST_UTF16_UNITS * 3,
        ),
    )?;
    writer.write_u16(handshake.server_port);
    writer.write_var_int(STATUS_NEXT_STATE);
    encode_frame(writer.as_slice(), MAX_STATUS_FRAME_SIZE)
}

pub fn encode_status_request() -> Result<Vec<u8>, CodecError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(STATUS_REQUEST_PACKET_ID);
    encode_frame(writer.as_slice(), MAX_STATUS_FRAME_SIZE)
}

pub fn decode_status_response(
    frame: &[u8],
    limits: StatusJsonLimits,
) -> Result<StatusResponse, StatusProtocolError> {
    let packet = split_raw_packet(frame)?;
    require_packet_id(STATUS_RESPONSE_PACKET_ID, packet.id)?;
    let mut reader = CodecReader::new(packet.payload);
    let json = reader.read_string(StringLimits::new(
        MAX_STATUS_JSON_UTF16_UNITS,
        MAX_STATUS_JSON_ENCODED_BYTES,
    ))?;
    require_consumed(&reader)?;
    parse_status_json(json, limits)
}

pub fn parse_status_json(
    json: &str,
    limits: StatusJsonLimits,
) -> Result<StatusResponse, StatusProtocolError> {
    let raw_value: Value =
        serde_json::from_str(json).map_err(|error| StatusProtocolError::MalformedJson {
            line: error.line(),
            column: error.column(),
        })?;
    let wire: WireStatusResponse =
        serde_json::from_value(raw_value).map_err(|_| StatusProtocolError::InvalidStatusData)?;
    if wire.players.online < 0 || wire.players.max < 0 {
        return Err(StatusProtocolError::InvalidPlayerCounts {
            online: wire.players.online,
            max: wire.players.max,
        });
    }
    if wire.players.sample.len() > limits.max_sample_entries {
        return Err(StatusProtocolError::PlayerSampleTooLarge {
            count: wire.players.sample.len(),
            max: limits.max_sample_entries,
        });
    }
    check_text(
        "version name",
        &wire.version.name,
        limits.max_text_field_bytes,
    )?;
    for sample in &wire.players.sample {
        check_text(
            "player sample name",
            &sample.name,
            limits.max_text_field_bytes,
        )?;
        check_text("player sample ID", &sample.id, limits.max_text_field_bytes)?;
    }
    if let Some(favicon) = &wire.favicon {
        check_text("favicon", favicon, limits.max_favicon_bytes)?;
    }

    Ok(StatusResponse {
        version: StatusVersion {
            name: wire.version.name,
            protocol: wire.version.protocol,
        },
        players: StatusPlayers {
            max: wire.players.max,
            online: wire.players.online,
            sample: wire
                .players
                .sample
                .into_iter()
                .map(|sample| StatusPlayerSample {
                    name: sample.name,
                    id: sample.id,
                })
                .collect(),
        },
        description: wire.description,
        favicon: wire.favicon,
        additional_fields: wire.additional_fields,
        raw_json: json.to_owned(),
    })
}

pub fn encode_status_ping(nonce: i64) -> Result<Vec<u8>, CodecError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PING_PACKET_ID);
    writer.write_i64(nonce);
    encode_frame(writer.as_slice(), MAX_STATUS_FRAME_SIZE)
}

pub fn decode_status_pong(frame: &[u8], expected_nonce: i64) -> Result<(), StatusProtocolError> {
    let packet = split_raw_packet(frame)?;
    require_packet_id(PONG_PACKET_ID, packet.id)?;
    let mut reader = CodecReader::new(packet.payload);
    let actual = reader.read_i64()?;
    require_consumed(&reader)?;
    if actual != expected_nonce {
        return Err(StatusProtocolError::PongMismatch {
            expected: expected_nonce,
            actual,
        });
    }
    Ok(())
}

fn require_packet_id(expected: i32, actual: i32) -> Result<(), StatusProtocolError> {
    if actual == expected {
        Ok(())
    } else {
        Err(StatusProtocolError::UnexpectedPacketId { expected, actual })
    }
}

fn require_consumed(reader: &CodecReader<'_>) -> Result<(), StatusProtocolError> {
    if reader.remaining() == 0 {
        Ok(())
    } else {
        Err(StatusProtocolError::TrailingPacketData {
            remaining: reader.remaining(),
        })
    }
}

fn check_text(field: &'static str, value: &str, max: usize) -> Result<(), StatusProtocolError> {
    if value.len() > max {
        Err(StatusProtocolError::TextFieldTooLarge {
            field,
            bytes: value.len(),
            max,
        })
    } else {
        Ok(())
    }
}
