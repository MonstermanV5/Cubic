//! Narrow Minecraft Java 26.1.2 / protocol 775 Login and Configuration profile.
//!
//! This module is temporary Phase 7 bootstrap data. Phase 12 will replace its
//! manually maintained packet IDs and shapes with generated packet codecs.

use thiserror::Error;

mod chunk;
pub use chunk::{
    ChunkDecodeError, LevelChunkWithLight, LightUpdate, WireBlockEntity, WireChunkSection,
    WireHeightmap, WireLightData, WirePalettedContainer,
};

use crate::{
    BitSetLimits, CodecError, CodecReader, CodecWriter, MINECRAFT_MAX_FRAME_SIZE, ProtocolUuid,
    StringLimits, encode_frame,
    nbt::{
        NbtCompound, NbtError, NbtLimits, NbtTag, decode_unnamed_network_root_complete,
        decode_unnamed_network_tag,
    },
    packet_schema::{
        FieldCodec, PacketDirection, PacketIdentityCheck, PacketLayout, PacketRegistry,
        PacketSchemaError, ProtocolState,
    },
    split_raw_packet,
};

pub const MINECRAFT_VERSION_ID: &str = "26.1.2";
pub const PROTOCOL_VERSION: i32 = 775;
pub const MAX_BOOTSTRAP_FRAME_SIZE: usize = MINECRAFT_MAX_FRAME_SIZE;
pub const MAX_CONFIGURATION_BUFFERED_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_USERNAME_UTF16_UNITS: usize = 16;
pub const MAX_LOGIN_PROPERTIES: usize = 64;
pub const MAX_LOGIN_PLUGIN_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_CONFIGURATION_CUSTOM_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_CONFIGURATION_REGISTRY_ENTRIES: usize = 4_096;
pub const MAX_PLAY_CUSTOM_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_DISCONNECT_BYTES: usize = 32 * 1024;
pub const MAX_KNOWN_PACKS: usize = 64;
pub const MAX_CHAT_UTF16_UNITS: usize = 256;
pub const MAX_CHAT_COMPONENT_BYTES: usize = 32 * 1024;
pub const MAX_LAST_SEEN_MESSAGES: usize = 20;
pub const CHAT_ACKNOWLEDGEMENT_BYTES: usize = 3;
pub const PLAYER_CHAT_SIGNATURE_BYTES: usize = 256;
pub const MAX_PLAYER_PUBLIC_KEY_BYTES: usize = 512;
pub const MAX_PLAYER_KEY_SIGNATURE_BYTES: usize = 4096;
pub const MAX_KNOWN_DIMENSIONS: usize = 1_024;
pub const MAX_WORLD_CLOCKS: usize = 64;
pub const MAX_SECTION_BLOCK_UPDATES: usize = 4_096;

const LOGIN_START_ID: i32 = 0x00;
const LOGIN_DISCONNECT_ID: i32 = 0x00;
const LOGIN_ENCRYPTION_REQUEST_ID: i32 = 0x01;
const LOGIN_SUCCESS_ID: i32 = 0x02;
const LOGIN_SET_COMPRESSION_ID: i32 = 0x03;
const LOGIN_PLUGIN_REQUEST_ID: i32 = 0x04;
const LOGIN_COOKIE_REQUEST_ID: i32 = 0x05;
const LOGIN_PLUGIN_RESPONSE_ID: i32 = 0x02;
const LOGIN_ACKNOWLEDGED_ID: i32 = 0x03;
const LOGIN_COOKIE_RESPONSE_ID: i32 = 0x04;

const CONFIG_CLIENT_INFORMATION_ID: i32 = 0x00;
const CONFIG_COOKIE_RESPONSE_ID: i32 = 0x01;
const CONFIG_FINISH_ACK_ID: i32 = 0x03;
const CONFIG_KEEP_ALIVE_RESPONSE_ID: i32 = 0x04;
const CONFIG_PONG_ID: i32 = 0x05;
const CONFIG_KNOWN_PACKS_RESPONSE_ID: i32 = 0x07;

const CONFIG_COOKIE_REQUEST_ID: i32 = 0x00;
const CONFIG_CUSTOM_PAYLOAD_ID: i32 = 0x01;
const CONFIG_DISCONNECT_ID: i32 = 0x02;
const CONFIG_FINISH_ID: i32 = 0x03;
const CONFIG_KEEP_ALIVE_ID: i32 = 0x04;
const CONFIG_PING_ID: i32 = 0x05;
const CONFIG_RESET_CHAT_ID: i32 = 0x06;
const CONFIG_REGISTRY_DATA_ID: i32 = 0x07;
const CONFIG_RESOURCE_PACK_POP_ID: i32 = 0x08;
const CONFIG_RESOURCE_PACK_PUSH_ID: i32 = 0x09;
const CONFIG_STORE_COOKIE_ID: i32 = 0x0a;
const CONFIG_TRANSFER_ID: i32 = 0x0b;
const CONFIG_ENABLED_FEATURES_ID: i32 = 0x0c;
const CONFIG_UPDATE_TAGS_ID: i32 = 0x0d;
const CONFIG_KNOWN_PACKS_ID: i32 = 0x0e;
const CONFIG_REPORT_DETAILS_ID: i32 = 0x0f;
const CONFIG_SERVER_LINKS_ID: i32 = 0x10;
const CONFIG_CLEAR_DIALOG_ID: i32 = 0x11;
const CONFIG_SHOW_DIALOG_ID: i32 = 0x12;
const CONFIG_CODE_OF_CONDUCT_ID: i32 = 0x13;

const INITIAL_PLAY_LOGIN_ID: i32 = 0x31;

const PLAY_CONFIRM_TELEPORT_ID: i32 = 0x00;
const PLAY_CHAT_ACKNOWLEDGEMENT_ID: i32 = 0x06;
const PLAY_CHAT_MESSAGE_ID: i32 = 0x09;
const PLAY_CHAT_SESSION_UPDATE_ID: i32 = 0x0a;
const PLAY_CHUNK_BATCH_RECEIVED_ID: i32 = 0x0b;
const PLAY_CLIENT_TICK_END_ID: i32 = 0x0d;
const PLAY_CLIENT_INFORMATION_ID: i32 = 0x0e;
const PLAY_ACKNOWLEDGE_CONFIGURATION_ID: i32 = 0x10;
const PLAY_COOKIE_RESPONSE_ID: i32 = 0x15;
const PLAY_KEEP_ALIVE_RESPONSE_ID: i32 = 0x1c;
const PLAY_MOVE_PLAYER_POS_ID: i32 = 0x1e;
const PLAY_MOVE_PLAYER_POS_ROT_ID: i32 = 0x1f;
const PLAY_MOVE_PLAYER_ROT_ID: i32 = 0x20;
const PLAY_MOVE_PLAYER_STATUS_ONLY_ID: i32 = 0x21;
const PLAY_PLAYER_ABILITIES_ID: i32 = 0x28;
const PLAY_PLAYER_COMMAND_ID: i32 = 0x2a;
const PLAY_PLAYER_INPUT_ID: i32 = 0x2b;
const PLAY_PLAYER_LOADED_ID: i32 = 0x2c;
const PLAY_PONG_ID: i32 = 0x2d;
const PLAY_PICK_ITEM_FROM_BLOCK_ID: i32 = 0x24;
const PLAY_PICK_ITEM_FROM_ENTITY_ID: i32 = 0x25;
const PLAY_PLAYER_ACTION_ID: i32 = 0x29;
const PLAY_SWING_ID: i32 = 0x3f;
const PLAY_USE_ITEM_ON_ID: i32 = 0x42;
const PLAY_USE_ITEM_ID: i32 = 0x43;

const PLAY_CHANGE_DIFFICULTY_ID: i32 = 0x0a;
const PLAY_BLOCK_CHANGED_ACK_ID: i32 = 0x04;
const PLAY_BLOCK_UPDATE_ID: i32 = 0x08;
const PLAY_CHUNK_BATCH_FINISHED_ID: i32 = 0x0b;
const PLAY_CHUNK_BATCH_START_ID: i32 = 0x0c;
const PLAY_COOKIE_REQUEST_ID: i32 = 0x15;
const PLAY_CUSTOM_PAYLOAD_ID: i32 = 0x18;
const PLAY_GAME_EVENT_ID: i32 = 0x26;
const PLAY_FORGET_LEVEL_CHUNK_ID: i32 = 0x25;
const PLAY_INITIALIZE_BORDER_ID: i32 = 0x2b;
const PLAY_LEVEL_CHUNK_WITH_LIGHT_ID: i32 = 0x2d;
const PLAY_LIGHT_UPDATE_ID: i32 = 0x30;
const PLAY_RESOURCE_PACK_PUSH_ID: i32 = 0x51;
const PLAY_TRANSFER_ID: i32 = 0x81;
const PLAY_DISCONNECT_ID: i32 = 0x20;
const PLAY_DISGUISED_CHAT_ID: i32 = 0x21;
const PLAY_KEEP_ALIVE_ID: i32 = 0x2c;
const PLAY_PING_ID: i32 = 0x3d;
const PLAY_PLAYER_CHAT_ID: i32 = 0x41;
const PLAY_CLIENTBOUND_PLAYER_ABILITIES_ID: i32 = 0x40;
const PLAY_PLAYER_POSITION_ID: i32 = 0x48;
const PLAY_PLAYER_ROTATION_ID: i32 = 0x49;
const PLAY_RESPAWN_ID: i32 = 0x52;
const PLAY_SECTION_BLOCKS_UPDATE_ID: i32 = 0x54;
const PLAY_SET_DEFAULT_SPAWN_POSITION_ID: i32 = 0x61;
const PLAY_SET_ENTITY_DATA_ID: i32 = 0x63;
const PLAY_SET_HEALTH_ID: i32 = 0x68;
const PLAY_SET_ENTITY_MOTION_ID: i32 = 0x65;
const PLAY_SET_TIME_ID: i32 = 0x71;
const PLAY_START_CONFIGURATION_ID: i32 = 0x76;
const PLAY_SYSTEM_CHAT_ID: i32 = 0x79;

const USERNAME_LIMITS: StringLimits = StringLimits::new(MAX_USERNAME_UTF16_UNITS, 48);
const IDENTIFIER_LIMITS: StringLimits = StringLimits::new(32_767, 32_767);
const DISCONNECT_LIMITS: StringLimits =
    StringLimits::new(MAX_DISCONNECT_BYTES, MAX_DISCONNECT_BYTES);
const PROPERTY_NAME_LIMITS: StringLimits = StringLimits::new(64, 192);
const PROPERTY_VALUE_LIMITS: StringLimits = StringLimits::new(16_384, 49_152);
const KNOWN_PACK_NAMESPACE_LIMITS: StringLimits = StringLimits::new(64, 192);
const KNOWN_PACK_ID_LIMITS: StringLimits = StringLimits::new(256, 768);
const KNOWN_PACK_VERSION_LIMITS: StringLimits = StringLimits::new(128, 384);
const CHAT_LIMITS: StringLimits = StringLimits::new(MAX_CHAT_UTF16_UNITS, 768);
const RESOURCE_LOCATION_LIMITS: StringLimits = StringLimits::new(256, 768);

/// Packet identities/IDs already proven by Cubic's protocol-775 bootstrap.
///
/// Phase 12 compares these facts with the official Data Generator report. The
/// list is not a replacement schema: field layouts remain in this temporary
/// bootstrap module until a lawful structural source is available.
#[must_use]
pub const fn packet_identity_cross_checks() -> &'static [PacketIdentityCheck] {
    use PacketDirection::{Clientbound as C, Serverbound as S};
    use ProtocolState::{Configuration as Config, Handshake, Login, Play, Status};
    &[
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:block_update",
            id: PLAY_BLOCK_UPDATE_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:block_changed_ack",
            id: PLAY_BLOCK_CHANGED_ACK_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:pick_item_from_block",
            id: PLAY_PICK_ITEM_FROM_BLOCK_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:pick_item_from_entity",
            id: PLAY_PICK_ITEM_FROM_ENTITY_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:player_action",
            id: PLAY_PLAYER_ACTION_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:swing",
            id: PLAY_SWING_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:use_item_on",
            id: PLAY_USE_ITEM_ON_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:use_item",
            id: PLAY_USE_ITEM_ID as u32,
        },
        PacketIdentityCheck {
            state: Handshake,
            direction: S,
            identity: "minecraft:intention",
            id: 0,
        },
        PacketIdentityCheck {
            state: Status,
            direction: S,
            identity: "minecraft:status_request",
            id: 0,
        },
        PacketIdentityCheck {
            state: Status,
            direction: C,
            identity: "minecraft:status_response",
            id: 0,
        },
        PacketIdentityCheck {
            state: Status,
            direction: S,
            identity: "minecraft:ping_request",
            id: 1,
        },
        PacketIdentityCheck {
            state: Status,
            direction: C,
            identity: "minecraft:pong_response",
            id: 1,
        },
        PacketIdentityCheck {
            state: Login,
            direction: S,
            identity: "minecraft:hello",
            id: LOGIN_START_ID as u32,
        },
        PacketIdentityCheck {
            state: Login,
            direction: C,
            identity: "minecraft:hello",
            id: LOGIN_ENCRYPTION_REQUEST_ID as u32,
        },
        PacketIdentityCheck {
            state: Login,
            direction: C,
            identity: "minecraft:login_finished",
            id: LOGIN_SUCCESS_ID as u32,
        },
        PacketIdentityCheck {
            state: Login,
            direction: C,
            identity: "minecraft:login_compression",
            id: LOGIN_SET_COMPRESSION_ID as u32,
        },
        PacketIdentityCheck {
            state: Login,
            direction: S,
            identity: "minecraft:login_acknowledged",
            id: LOGIN_ACKNOWLEDGED_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: S,
            identity: "minecraft:client_information",
            id: CONFIG_CLIENT_INFORMATION_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: C,
            identity: "minecraft:custom_payload",
            id: CONFIG_CUSTOM_PAYLOAD_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: C,
            identity: "minecraft:disconnect",
            id: CONFIG_DISCONNECT_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: C,
            identity: "minecraft:finish_configuration",
            id: CONFIG_FINISH_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: S,
            identity: "minecraft:finish_configuration",
            id: CONFIG_FINISH_ACK_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: C,
            identity: "minecraft:keep_alive",
            id: CONFIG_KEEP_ALIVE_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: C,
            identity: "minecraft:ping",
            id: CONFIG_PING_ID as u32,
        },
        PacketIdentityCheck {
            state: Config,
            direction: C,
            identity: "minecraft:select_known_packs",
            id: CONFIG_KNOWN_PACKS_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:accept_teleportation",
            id: PLAY_CONFIRM_TELEPORT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:chat_ack",
            id: PLAY_CHAT_ACKNOWLEDGEMENT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:chat",
            id: PLAY_CHAT_MESSAGE_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:chat_session_update",
            id: PLAY_CHAT_SESSION_UPDATE_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:chunk_batch_received",
            id: PLAY_CHUNK_BATCH_RECEIVED_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:configuration_acknowledged",
            id: PLAY_ACKNOWLEDGE_CONFIGURATION_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:move_player_pos",
            id: PLAY_MOVE_PLAYER_POS_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:move_player_pos_rot",
            id: PLAY_MOVE_PLAYER_POS_ROT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:move_player_rot",
            id: PLAY_MOVE_PLAYER_ROT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:move_player_status_only",
            id: PLAY_MOVE_PLAYER_STATUS_ONLY_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:player_abilities",
            id: PLAY_PLAYER_ABILITIES_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:player_command",
            id: PLAY_PLAYER_COMMAND_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: S,
            identity: "minecraft:player_input",
            id: PLAY_PLAYER_INPUT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:custom_payload",
            id: PLAY_CUSTOM_PAYLOAD_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:forget_level_chunk",
            id: PLAY_FORGET_LEVEL_CHUNK_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:level_chunk_with_light",
            id: PLAY_LEVEL_CHUNK_WITH_LIGHT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:light_update",
            id: PLAY_LIGHT_UPDATE_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:disconnect",
            id: PLAY_DISCONNECT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:disguised_chat",
            id: PLAY_DISGUISED_CHAT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:keep_alive",
            id: PLAY_KEEP_ALIVE_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:login",
            id: INITIAL_PLAY_LOGIN_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:ping",
            id: PLAY_PING_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:player_abilities",
            id: PLAY_CLIENTBOUND_PLAYER_ABILITIES_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:player_chat",
            id: PLAY_PLAYER_CHAT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:set_health",
            id: PLAY_SET_HEALTH_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:set_entity_data",
            id: PLAY_SET_ENTITY_DATA_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:start_configuration",
            id: PLAY_START_CONFIGURATION_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:system_chat",
            id: PLAY_SYSTEM_CHAT_ID as u32,
        },
        PacketIdentityCheck {
            state: Play,
            direction: C,
            identity: "minecraft:section_blocks_update",
            id: PLAY_SECTION_BLOCKS_UPDATE_ID as u32,
        },
    ]
}

/// Cross-checks representative generated layouts against codecs that have
/// already passed real protocol-775 interoperability tests.
pub fn cross_check_generated_layouts(
    registry: &PacketRegistry,
) -> Result<usize, PacketSchemaError> {
    use PacketDirection::{Clientbound as C, Serverbound as S};
    use ProtocolState::{Configuration as Config, Handshake, Login, Play, Status};

    type FieldCheck = (&'static str, fn(&FieldCodec) -> bool);
    type LayoutCheck = (
        ProtocolState,
        PacketDirection,
        &'static str,
        &'static [FieldCheck],
    );
    let checks: &[LayoutCheck] = &[
        (
            Handshake,
            S,
            "minecraft:intention",
            &[
                ("protocol_version", is_varint),
                ("server_host", is_string),
                ("server_port", is_u16),
                ("next_state", is_varint),
            ],
        ),
        (Status, S, "minecraft:status_request", &[]),
        (
            Status,
            C,
            "minecraft:status_response",
            &[("response", is_string)],
        ),
        (Status, S, "minecraft:ping_request", &[("time", is_i64)]),
        (Status, C, "minecraft:pong_response", &[("time", is_i64)]),
        (
            Login,
            C,
            "minecraft:login_compression",
            &[("threshold", is_varint)],
        ),
        (Config, C, "minecraft:finish_configuration", &[]),
        (Config, S, "minecraft:finish_configuration", &[]),
        (
            Config,
            C,
            "minecraft:keep_alive",
            &[("keep_alive_id", is_i64)],
        ),
        (
            Play,
            C,
            "minecraft:keep_alive",
            &[("keep_alive_id", is_i64)],
        ),
        (
            Play,
            S,
            "minecraft:chat",
            &[
                ("message", is_string),
                ("timestamp", is_i64),
                ("salt", is_i64),
                ("signature", is_optional_signature),
                ("offset", is_varint),
                ("acknowledged", is_acknowledged),
                ("checksum", is_u8),
            ],
        ),
        (
            Play,
            S,
            "minecraft:chat_session_update",
            &[
                ("session_uuid", is_uuid),
                ("expire_time", is_i64),
                ("public_key", is_byte_array),
                ("signature", is_byte_array),
            ],
        ),
        (
            Play,
            C,
            "minecraft:system_chat",
            &[("content", is_nbt_tag), ("is_action_bar", is_bool)],
        ),
        (
            Play,
            C,
            "minecraft:custom_payload",
            &[("channel", is_string), ("data", is_remaining_bytes)],
        ),
    ];
    for (state, direction, identity, expected) in checks {
        let identity_value = cubic_version::MinecraftIdentifier::new(*identity)
            .map_err(|error| PacketSchemaError::InvalidSchema(error.to_string()))?;
        let packet = registry
            .by_identity(*state, *direction, &identity_value)
            .ok_or_else(|| {
                PacketSchemaError::InvalidSchema(format!(
                    "generated schema is missing structural cross-check packet {identity}"
                ))
            })?;
        let PacketLayout::Fields { fields } = &packet.layout else {
            return Err(PacketSchemaError::InvalidSchema(format!(
                "generated schema has no layout for structural cross-check packet {identity}"
            )));
        };
        if fields.len() != expected.len()
            || fields
                .iter()
                .zip(*expected)
                .any(|(field, (name, predicate))| field.name != *name || !predicate(&field.codec))
        {
            return Err(PacketSchemaError::InvalidSchema(format!(
                "generated layout disagrees with proven bootstrap codec for {identity}"
            )));
        }
    }
    Ok(checks.len())
}

fn is_bool(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::Bool)
}
fn is_u8(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::U8)
}
fn is_u16(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::U16)
}
fn is_i64(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::I64)
}
fn is_varint(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::VarInt)
}
fn is_string(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::String { .. })
}
fn is_uuid(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::Uuid)
}
fn is_byte_array(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::ByteArray { .. })
}
fn is_nbt_tag(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::NbtTag)
}
fn is_remaining_bytes(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::RemainingBytes { .. })
}
fn is_optional_signature(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::Optional { value } if matches!(value.as_ref(), FieldCodec::FixedBytes { length: 256 }))
}
fn is_acknowledged(codec: &FieldCodec) -> bool {
    matches!(codec, FieldCodec::FixedBytes { length: 3 })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginSuccess<'a> {
    pub uuid: ProtocolUuid,
    pub username: &'a str,
    pub properties: Vec<GameProfileProperty<'a>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncryptionRequest<'a> {
    pub server_id: &'a str,
    pub public_key_der: &'a [u8],
    pub verify_token: &'a [u8],
    pub should_authenticate: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameProfileProperty<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub signature: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownPack<'a> {
    pub namespace: &'a str,
    pub id: &'a str,
    pub version: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginClientbound<'a> {
    Disconnect {
        reason_json: &'a str,
    },
    EncryptionRequest(EncryptionRequest<'a>),
    Success(LoginSuccess<'a>),
    SetCompression {
        threshold: i32,
    },
    PluginRequest {
        transaction_id: i32,
        channel: &'a str,
        payload: &'a [u8],
    },
    CookieRequest {
        key: &'a str,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkippedConfigurationPacket {
    ResetChat,
    RegistryData,
    ResourcePackPop,
    StoreCookie,
    EnabledFeatures,
    UpdateTags,
    ReportDetails,
    ServerLinks,
    ClearDialog,
    ShowDialog,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationClientbound<'a> {
    CookieRequest {
        key: &'a str,
    },
    CustomPayload {
        channel: &'a str,
        payload: &'a [u8],
    },
    Disconnect {
        reason: NbtCompound,
    },
    Finish,
    KeepAlive {
        id: i64,
    },
    Ping {
        id: i32,
    },
    RegistryData {
        registry: &'a str,
        entries: Vec<ConfigurationRegistryEntry<'a>>,
    },
    KnownPacks(Vec<KnownPack<'a>>),
    ResourcePackPush,
    Transfer,
    CodeOfConduct,
    Skipped {
        packet: SkippedConfigurationPacket,
        payload_bytes: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationRegistryEntry<'a> {
    pub identifier: &'a str,
    pub data: Option<NbtTag>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextComponent {
    pub value: NbtTag,
    pub plain_text: String,
}

/// Protocol 775's fixed last-seen update embedded in an outgoing chat message.
///
/// The acknowledgement bytes are a Java `BitSet` encoding of exactly 20 bits:
/// bit zero is the least-significant bit of byte zero, and the high four bits of
/// byte two are unused. A checksum of zero disables checksum verification, as
/// required by Cubic's unsigned Phase 8 development profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatLastSeenUpdate {
    offset: i32,
    acknowledged: [u8; CHAT_ACKNOWLEDGEMENT_BYTES],
    checksum: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageSignature(Box<[u8; PLAYER_CHAT_SIGNATURE_BYTES]>);

impl MessageSignature {
    #[must_use]
    pub fn new(bytes: [u8; PLAYER_CHAT_SIGNATURE_BYTES]) -> Self {
        Self(Box::new(bytes))
    }

    #[must_use]
    pub fn bytes(&self) -> [u8; PLAYER_CHAT_SIGNATURE_BYTES] {
        *self.0
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PLAYER_CHAT_SIGNATURE_BYTES] {
        &self.0
    }
}

impl ChatLastSeenUpdate {
    #[must_use]
    pub const fn empty_with_disabled_checksum() -> Self {
        Self {
            offset: 0,
            acknowledged: [0; CHAT_ACKNOWLEDGEMENT_BYTES],
            checksum: 0,
        }
    }

    pub fn new(
        offset: i32,
        acknowledged: [u8; CHAT_ACKNOWLEDGEMENT_BYTES],
        checksum: u8,
    ) -> Result<Self, BootstrapProtocolError> {
        if offset < 0 {
            return Err(BootstrapProtocolError::NegativeCount {
                context: "Chat Message last-seen offset",
                value: offset,
            });
        }
        if acknowledged[2] & 0xf0 != 0 {
            return Err(CodecError::ValueOutOfRange {
                context: "Chat Message fixed 20-bit acknowledgement",
                value: i128::from(acknowledged[2]),
                min: 0,
                max: 0x0f,
            }
            .into());
        }
        Ok(Self {
            offset,
            acknowledged,
            checksum,
        })
    }

    #[must_use]
    pub const fn offset(self) -> i32 {
        self.offset
    }

    #[must_use]
    pub const fn acknowledged(self) -> [u8; CHAT_ACKNOWLEDGEMENT_BYTES] {
        self.acknowledged
    }

    #[must_use]
    pub const fn checksum(self) -> u8 {
        self.checksum
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayClientbound {
    BlockChangedAck {
        sequence: i32,
    },
    Login(InitialPlayLogin),
    KeepAlive {
        id: i64,
    },
    Ping {
        id: i32,
    },
    PlayerPosition(PlayerPosition),
    PlayerRotation(PlayerRotation),
    PlayerAbilities(PlayerAbilities),
    SetEntityMotion(EntityMotion),
    Respawn(Respawn),
    SetDefaultSpawnPosition(DefaultSpawnPosition),
    SetTime(WorldTime),
    ChangeDifficulty {
        difficulty: i32,
        locked: bool,
    },
    GameEvent {
        event: u8,
        value: f32,
    },
    InitializeBorder(InitializeBorder),
    ChunkBatchFinished {
        chunks: i32,
    },
    ChunkBatchStart,
    LevelChunkWithLight(LevelChunkWithLight),
    ForgetLevelChunk {
        x: i32,
        z: i32,
    },
    LightUpdate(LightUpdate),
    BlockUpdate(BlockUpdate),
    SectionBlocksUpdate(SectionBlocksUpdate),
    CookieRequest {
        key: String,
    },
    CustomPayload {
        channel: String,
        payload_bytes: usize,
    },
    PlayerChat {
        sender_uuid: ProtocolUuid,
        sender_name: String,
        signed_content: String,
        unsigned_content: Option<TextComponent>,
        message: TextComponent,
        global_index: i32,
        sender_index: i32,
        signature: Option<MessageSignature>,
        modified: bool,
    },
    DisguisedChat {
        sender_name: String,
        message: TextComponent,
    },
    SystemChat {
        message: TextComponent,
        overlay: bool,
    },
    Disconnect {
        reason: TextComponent,
    },
    Health {
        health: f32,
    },
    /// Bounded projection of the current entity-metadata packet. Phase 18
    /// extracts only the base Entity air-supply field (index 1, INT serializer
    /// 1); all remaining metadata stays opaque and is never retained.
    EntityData {
        entity_id: i32,
        air_supply: Option<i32>,
        payload_bytes: usize,
    },
    StartConfiguration,
    ResourcePackPush,
    Transfer,
    Ignored {
        packet_id: i32,
        payload_bytes: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayDecodeWork {
    Normal,
    ChunkHeavy,
}

/// Classifies only decode cost; packet identity remains confined to this
/// exact-version profile. Networking can keep its 20 Hz control plane alive
/// while CPU-heavy chunk payloads are decoded on a worker.
pub fn classify_play_decode_work(
    frame_body: &[u8],
) -> Result<PlayDecodeWork, BootstrapProtocolError> {
    let packet = split_raw_packet(frame_body)?;
    Ok(if packet.id == PLAY_LEVEL_CHUNK_WITH_LIGHT_ID {
        PlayDecodeWork::ChunkHeavy
    } else {
        PlayDecodeWork::Normal
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockUpdate {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub state_id: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionBlocksUpdate {
    pub section_x: i32,
    pub section_y: i32,
    pub section_z: i32,
    pub updates: Vec<SectionBlockUpdate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SectionBlockUpdate {
    pub local_x: u8,
    pub local_y: u8,
    pub local_z: u8,
    pub state_id: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalPosition {
    pub dimension: String,
    pub position: crate::BlockPosition,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpawnInfo {
    pub dimension_type_raw_id: i32,
    pub dimension: String,
    pub hashed_seed: i64,
    pub game_mode: i8,
    pub previous_game_mode: u8,
    pub debug_world: bool,
    pub flat_world: bool,
    pub last_death_location: Option<GlobalPosition>,
    pub portal_cooldown_ticks: i32,
    pub sea_level: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitialPlayLogin {
    pub player_entity_id: i32,
    pub hardcore: bool,
    pub known_dimensions: Vec<String>,
    pub max_players: i32,
    pub view_distance: i32,
    pub simulation_distance: i32,
    pub reduced_debug_info: bool,
    pub show_death_screen: bool,
    pub limited_crafting: bool,
    pub spawn: SpawnInfo,
    pub secure_chat_enforced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerPosition {
    pub teleport_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub relative_flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerRotation {
    pub yaw: f32,
    pub relative_yaw: bool,
    pub pitch: f32,
    pub relative_pitch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntityMotion {
    pub entity_id: i32,
    pub delta_x: f64,
    pub delta_y: f64,
    pub delta_z: f64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlayerInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sneak: bool,
    pub sprint: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerAbilities {
    pub invulnerable: bool,
    pub flying: bool,
    pub may_fly: bool,
    pub instant_build: bool,
    pub flying_speed: f32,
    pub walking_speed: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerCommandAction {
    StartSprinting,
    StopSprinting,
}

/// Direction data values used by the protocol-775 interaction packets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BlockFace {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerAction {
    StartDestroyBlock,
    AbortDestroyBlock,
    StopDestroyBlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractionHand {
    Main,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockHit {
    pub position: crate::BlockPosition,
    pub face: BlockFace,
    pub location_x: f32,
    pub location_y: f32,
    pub location_z: f32,
    pub inside: bool,
    pub world_border_hit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Respawn {
    pub spawn: SpawnInfo,
    pub data_to_keep: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DefaultSpawnPosition {
    pub position: GlobalPosition,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldClock {
    pub clock_type_raw_id: i32,
    pub total_ticks: i64,
    pub partial_tick: f32,
    pub rate: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorldTime {
    pub game_time: i64,
    pub clocks: Vec<WorldClock>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InitializeBorder {
    pub center_x: f64,
    pub center_z: f64,
    pub old_diameter: f64,
    pub new_diameter: f64,
    pub lerp_millis: i64,
    pub absolute_max_size: i32,
    pub warning_blocks: i32,
    pub warning_seconds: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientInformation<'a> {
    pub locale: &'a str,
    pub view_distance: i8,
    pub chat_mode: i32,
    pub chat_colors: bool,
    pub displayed_skin_parts: u8,
    pub main_hand: i32,
    pub text_filtering: bool,
    pub allows_server_listing: bool,
    pub particle_status: i32,
}

impl Default for ClientInformation<'static> {
    fn default() -> Self {
        Self {
            locale: "en_us",
            view_distance: 8,
            chat_mode: 0,
            chat_colors: true,
            displayed_skin_parts: 0x7f,
            main_hand: 1,
            text_filtering: false,
            allows_server_listing: true,
            particle_status: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum BootstrapProtocolError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("malformed bounded NBT disconnect reason")]
    Nbt(#[source] NbtError),
    #[error(transparent)]
    Chunk(#[from] ChunkDecodeError),
    #[error("unexpected packet ID {id} in {state} state")]
    UnexpectedPacketId { state: &'static str, id: i32 },
    #[error("{context} has {remaining} trailing bytes")]
    TrailingData {
        context: &'static str,
        remaining: usize,
    },
    #[error("negative {context} count {value}")]
    NegativeCount { context: &'static str, value: i32 },
    #[error("{context} count {count} exceeds limit {max}")]
    CountTooLarge {
        context: &'static str,
        count: usize,
        max: usize,
    },
    #[error("{context} payload has {length} bytes, exceeding limit {max}")]
    PayloadTooLarge {
        context: &'static str,
        length: usize,
        max: usize,
    },
    #[error("{context} contains a non-finite number")]
    NonFinite { context: &'static str },
}

pub fn encode_login_start(
    username: &str,
    uuid: ProtocolUuid,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(LOGIN_START_ID);
    writer.write_string(username, USERNAME_LIMITS)?;
    writer.write_uuid(uuid);
    frame(writer)
}

pub fn encode_login_acknowledged() -> Result<Vec<u8>, BootstrapProtocolError> {
    packet_without_payload(LOGIN_ACKNOWLEDGED_ID)
}

pub fn encode_encryption_response(
    encrypted_secret: &[u8],
    encrypted_verify_token: &[u8],
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(0x01);
    writer.write_byte_array(encrypted_secret, 4 * 1024)?;
    writer.write_byte_array(encrypted_verify_token, 4 * 1024)?;
    frame(writer)
}

pub fn encode_login_plugin_response(
    transaction_id: i32,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(LOGIN_PLUGIN_RESPONSE_ID);
    writer.write_var_int(transaction_id);
    writer.write_bool(false);
    frame(writer)
}

pub fn encode_login_cookie_response(key: &str) -> Result<Vec<u8>, BootstrapProtocolError> {
    encode_empty_cookie_response(LOGIN_COOKIE_RESPONSE_ID, key)
}

pub fn decode_login_clientbound(
    frame_body: &[u8],
) -> Result<LoginClientbound<'_>, BootstrapProtocolError> {
    let packet = split_raw_packet(frame_body)?;
    let mut reader = CodecReader::new(packet.payload);
    match packet.id {
        LOGIN_DISCONNECT_ID => {
            let reason_json = reader.read_string(DISCONNECT_LIMITS)?;
            require_consumed(&reader, "Login Disconnect")?;
            Ok(LoginClientbound::Disconnect { reason_json })
        }
        LOGIN_ENCRYPTION_REQUEST_ID => {
            let request = EncryptionRequest {
                server_id: reader.read_string(StringLimits::new(20, 60))?,
                public_key_der: reader.read_byte_array(4 * 1024)?,
                verify_token: reader.read_byte_array(64)?,
                should_authenticate: reader.read_bool()?,
            };
            require_consumed(&reader, "Encryption Request")?;
            Ok(LoginClientbound::EncryptionRequest(request))
        }
        LOGIN_SUCCESS_ID => decode_login_success(&mut reader).map(LoginClientbound::Success),
        LOGIN_SET_COMPRESSION_ID => {
            let threshold = reader.read_var_int()?;
            require_consumed(&reader, "Set Compression")?;
            Ok(LoginClientbound::SetCompression { threshold })
        }
        LOGIN_PLUGIN_REQUEST_ID => {
            let transaction_id = reader.read_var_int()?;
            let channel = reader.read_string(IDENTIFIER_LIMITS)?;
            let payload = reader.read_remaining();
            check_payload(
                "Login Plugin Request",
                payload,
                MAX_LOGIN_PLUGIN_PAYLOAD_BYTES,
            )?;
            Ok(LoginClientbound::PluginRequest {
                transaction_id,
                channel,
                payload,
            })
        }
        LOGIN_COOKIE_REQUEST_ID => {
            let key = reader.read_string(IDENTIFIER_LIMITS)?;
            require_consumed(&reader, "Login Cookie Request")?;
            Ok(LoginClientbound::CookieRequest { key })
        }
        id => Err(BootstrapProtocolError::UnexpectedPacketId { state: "Login", id }),
    }
}

pub fn encode_client_information(
    information: &ClientInformation<'_>,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(CONFIG_CLIENT_INFORMATION_ID);
    write_client_information_fields(&mut writer, information)?;
    frame(writer)
}

fn write_client_information_fields(
    writer: &mut CodecWriter,
    information: &ClientInformation<'_>,
) -> Result<(), BootstrapProtocolError> {
    writer.write_string(information.locale, StringLimits::new(16, 48))?;
    writer.write_i8(information.view_distance);
    writer.write_var_int(information.chat_mode);
    writer.write_bool(information.chat_colors);
    writer.write_u8(information.displayed_skin_parts);
    writer.write_var_int(information.main_hand);
    writer.write_bool(information.text_filtering);
    writer.write_bool(information.allows_server_listing);
    writer.write_var_int(information.particle_status);
    Ok(())
}

pub fn encode_configuration_cookie_response(key: &str) -> Result<Vec<u8>, BootstrapProtocolError> {
    encode_empty_cookie_response(CONFIG_COOKIE_RESPONSE_ID, key)
}

pub fn encode_configuration_keep_alive(id: i64) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(CONFIG_KEEP_ALIVE_RESPONSE_ID);
    writer.write_i64(id);
    frame(writer)
}

pub fn encode_configuration_pong(id: i32) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(CONFIG_PONG_ID);
    writer.write_i32(id);
    frame(writer)
}

pub fn encode_known_packs_response_empty() -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(CONFIG_KNOWN_PACKS_RESPONSE_ID);
    writer.write_var_int(0);
    frame(writer)
}

pub fn encode_finish_configuration() -> Result<Vec<u8>, BootstrapProtocolError> {
    packet_without_payload(CONFIG_FINISH_ACK_ID)
}

pub fn decode_configuration_clientbound(
    frame_body: &[u8],
) -> Result<ConfigurationClientbound<'_>, BootstrapProtocolError> {
    let packet = split_raw_packet(frame_body)?;
    let mut reader = CodecReader::new(packet.payload);
    match packet.id {
        CONFIG_COOKIE_REQUEST_ID => {
            let key = reader.read_string(IDENTIFIER_LIMITS)?;
            require_consumed(&reader, "Configuration Cookie Request")?;
            Ok(ConfigurationClientbound::CookieRequest { key })
        }
        CONFIG_CUSTOM_PAYLOAD_ID => {
            let channel = reader.read_string(IDENTIFIER_LIMITS)?;
            let payload = reader.read_remaining();
            check_payload(
                "Configuration Custom Payload",
                payload,
                MAX_CONFIGURATION_CUSTOM_PAYLOAD_BYTES,
            )?;
            Ok(ConfigurationClientbound::CustomPayload { channel, payload })
        }
        CONFIG_DISCONNECT_ID => decode_configuration_disconnect(packet.payload),
        CONFIG_FINISH_ID => {
            require_consumed(&reader, "Finish Configuration")?;
            Ok(ConfigurationClientbound::Finish)
        }
        CONFIG_KEEP_ALIVE_ID => {
            let id = reader.read_i64()?;
            require_consumed(&reader, "Configuration Keep Alive")?;
            Ok(ConfigurationClientbound::KeepAlive { id })
        }
        CONFIG_PING_ID => {
            let id = reader.read_i32()?;
            require_consumed(&reader, "Configuration Ping")?;
            Ok(ConfigurationClientbound::Ping { id })
        }
        CONFIG_RESET_CHAT_ID => {
            decode_empty_skipped(&reader, SkippedConfigurationPacket::ResetChat, "Reset Chat")
        }
        CONFIG_REGISTRY_DATA_ID => decode_configuration_registry_data(&mut reader),
        CONFIG_RESOURCE_PACK_POP_ID => Ok(skipped(
            SkippedConfigurationPacket::ResourcePackPop,
            packet.payload.len(),
        )),
        CONFIG_RESOURCE_PACK_PUSH_ID => Ok(ConfigurationClientbound::ResourcePackPush),
        CONFIG_STORE_COOKIE_ID => Ok(skipped(
            SkippedConfigurationPacket::StoreCookie,
            packet.payload.len(),
        )),
        CONFIG_TRANSFER_ID => Ok(ConfigurationClientbound::Transfer),
        CONFIG_ENABLED_FEATURES_ID => Ok(skipped(
            SkippedConfigurationPacket::EnabledFeatures,
            packet.payload.len(),
        )),
        CONFIG_UPDATE_TAGS_ID => Ok(skipped(
            SkippedConfigurationPacket::UpdateTags,
            packet.payload.len(),
        )),
        CONFIG_KNOWN_PACKS_ID => decode_known_packs(&mut reader),
        CONFIG_REPORT_DETAILS_ID => Ok(skipped(
            SkippedConfigurationPacket::ReportDetails,
            packet.payload.len(),
        )),
        CONFIG_SERVER_LINKS_ID => Ok(skipped(
            SkippedConfigurationPacket::ServerLinks,
            packet.payload.len(),
        )),
        CONFIG_CLEAR_DIALOG_ID => decode_empty_skipped(
            &reader,
            SkippedConfigurationPacket::ClearDialog,
            "Clear Dialog",
        ),
        CONFIG_SHOW_DIALOG_ID => Ok(skipped(
            SkippedConfigurationPacket::ShowDialog,
            packet.payload.len(),
        )),
        CONFIG_CODE_OF_CONDUCT_ID => Ok(ConfigurationClientbound::CodeOfConduct),
        id => Err(BootstrapProtocolError::UnexpectedPacketId {
            state: "Configuration",
            id,
        }),
    }
}

pub fn decode_play_clientbound(
    frame_body: &[u8],
) -> Result<PlayClientbound, BootstrapProtocolError> {
    let packet = split_raw_packet(frame_body)?;
    let mut reader = CodecReader::new(packet.payload);
    match packet.id {
        PLAY_BLOCK_CHANGED_ACK_ID => {
            let sequence = reader.read_var_int()?;
            if sequence < 0 {
                return Err(BootstrapProtocolError::NegativeCount {
                    context: "Block Changed Ack sequence",
                    value: sequence,
                });
            }
            require_consumed(&reader, "Block Changed Ack")?;
            Ok(PlayClientbound::BlockChangedAck { sequence })
        }
        INITIAL_PLAY_LOGIN_ID => decode_initial_play_login(&mut reader).map(PlayClientbound::Login),
        PLAY_CHANGE_DIFFICULTY_ID => {
            let difficulty = reader.read_var_int()?;
            let locked = reader.read_bool()?;
            require_consumed(&reader, "Change Difficulty")?;
            Ok(PlayClientbound::ChangeDifficulty { difficulty, locked })
        }
        PLAY_KEEP_ALIVE_ID => {
            let id = reader.read_i64()?;
            require_consumed(&reader, "Play Keep Alive")?;
            Ok(PlayClientbound::KeepAlive { id })
        }
        PLAY_PING_ID => {
            let id = reader.read_i32()?;
            require_consumed(&reader, "Play Ping")?;
            Ok(PlayClientbound::Ping { id })
        }
        PLAY_PLAYER_POSITION_ID => {
            decode_player_position(&mut reader).map(PlayClientbound::PlayerPosition)
        }
        PLAY_PLAYER_ROTATION_ID => {
            let result = PlayerRotation {
                yaw: reader.read_f32()?,
                relative_yaw: reader.read_bool()?,
                pitch: reader.read_f32()?,
                relative_pitch: reader.read_bool()?,
            };
            require_consumed(&reader, "Play Player Rotation")?;
            Ok(PlayClientbound::PlayerRotation(result))
        }
        PLAY_CLIENTBOUND_PLAYER_ABILITIES_ID => {
            let flags = reader.read_u8()?;
            if flags & !0x0f != 0 {
                return Err(CodecError::ValueOutOfRange {
                    context: "Player Abilities flags",
                    value: i128::from(flags),
                    min: 0,
                    max: 0x0f,
                }
                .into());
            }
            let flying_speed = reader.read_f32()?;
            let walking_speed = reader.read_f32()?;
            if !flying_speed.is_finite() || !walking_speed.is_finite() {
                return Err(BootstrapProtocolError::NonFinite {
                    context: "Player Abilities speed",
                });
            }
            require_consumed(&reader, "Player Abilities")?;
            Ok(PlayClientbound::PlayerAbilities(PlayerAbilities {
                invulnerable: flags & 0x01 != 0,
                flying: flags & 0x02 != 0,
                may_fly: flags & 0x04 != 0,
                instant_build: flags & 0x08 != 0,
                flying_speed,
                walking_speed,
            }))
        }
        PLAY_SET_ENTITY_MOTION_ID => {
            let result = EntityMotion {
                entity_id: reader.read_var_int()?,
                ..decode_low_precision_vec3(&mut reader)?
            };
            require_consumed(&reader, "Set Entity Motion")?;
            Ok(PlayClientbound::SetEntityMotion(result))
        }
        PLAY_RESPAWN_ID => decode_respawn(&mut reader).map(PlayClientbound::Respawn),
        PLAY_SET_DEFAULT_SPAWN_POSITION_ID => {
            decode_default_spawn_position(&mut reader).map(PlayClientbound::SetDefaultSpawnPosition)
        }
        PLAY_SET_TIME_ID => decode_world_time(&mut reader).map(PlayClientbound::SetTime),
        PLAY_GAME_EVENT_ID => {
            let event = reader.read_u8()?;
            let value = reader.read_f32()?;
            require_consumed(&reader, "Game Event")?;
            Ok(PlayClientbound::GameEvent { event, value })
        }
        PLAY_INITIALIZE_BORDER_ID => {
            decode_initialize_border(&mut reader).map(PlayClientbound::InitializeBorder)
        }
        PLAY_CHUNK_BATCH_FINISHED_ID => {
            let chunks = reader.read_var_int()?;
            if chunks < 0 {
                return Err(BootstrapProtocolError::NegativeCount {
                    context: "Play chunk batch",
                    value: chunks,
                });
            }
            require_consumed(&reader, "Play Chunk Batch Finished")?;
            Ok(PlayClientbound::ChunkBatchFinished { chunks })
        }
        PLAY_CHUNK_BATCH_START_ID => {
            require_consumed(&reader, "Play Chunk Batch Start")?;
            Ok(PlayClientbound::ChunkBatchStart)
        }
        PLAY_FORGET_LEVEL_CHUNK_ID => {
            let (x, z) = chunk::decode_forget_level_chunk(&mut reader)?;
            require_consumed(&reader, "Forget Level Chunk")?;
            Ok(PlayClientbound::ForgetLevelChunk { x, z })
        }
        PLAY_LEVEL_CHUNK_WITH_LIGHT_ID => {
            let chunk = chunk::decode_level_chunk_with_light(&mut reader)?;
            require_consumed(&reader, "Level Chunk With Light")?;
            Ok(PlayClientbound::LevelChunkWithLight(chunk))
        }
        PLAY_LIGHT_UPDATE_ID => {
            let update = chunk::decode_light_update(&mut reader)?;
            require_consumed(&reader, "Light Update")?;
            Ok(PlayClientbound::LightUpdate(update))
        }
        PLAY_BLOCK_UPDATE_ID => {
            let position = reader.read_block_position()?;
            let state_id = nonnegative_u32(reader.read_var_int()?, "Block Update state ID")?;
            require_consumed(&reader, "Block Update")?;
            Ok(PlayClientbound::BlockUpdate(BlockUpdate {
                x: position.x(),
                y: position.y(),
                z: position.z(),
                state_id,
            }))
        }
        PLAY_SECTION_BLOCKS_UPDATE_ID => {
            decode_section_blocks_update(&mut reader).map(PlayClientbound::SectionBlocksUpdate)
        }
        PLAY_COOKIE_REQUEST_ID => {
            let key = reader.read_string(IDENTIFIER_LIMITS)?.to_owned();
            require_consumed(&reader, "Play Cookie Request")?;
            Ok(PlayClientbound::CookieRequest { key })
        }
        PLAY_CUSTOM_PAYLOAD_ID => {
            let channel = reader.read_string(IDENTIFIER_LIMITS)?.to_owned();
            let payload = reader.read_remaining();
            check_payload(
                "Play Custom Payload",
                payload,
                MAX_PLAY_CUSTOM_PAYLOAD_BYTES,
            )?;
            Ok(PlayClientbound::CustomPayload {
                channel,
                payload_bytes: payload.len(),
            })
        }
        PLAY_SYSTEM_CHAT_ID => {
            let message = decode_text_component(&mut reader)?;
            let overlay = reader.read_bool()?;
            require_consumed(&reader, "System Chat")?;
            Ok(PlayClientbound::SystemChat { message, overlay })
        }
        PLAY_DISGUISED_CHAT_ID => {
            let message = decode_text_component(&mut reader)?;
            let sender_name = decode_bound_chat_type(&mut reader)?;
            require_consumed(&reader, "Disguised Chat")?;
            Ok(PlayClientbound::DisguisedChat {
                sender_name,
                message,
            })
        }
        PLAY_PLAYER_CHAT_ID => decode_player_chat(&mut reader),
        PLAY_DISCONNECT_ID => {
            let reason = decode_text_component(&mut reader)?;
            require_consumed(&reader, "Play Disconnect")?;
            Ok(PlayClientbound::Disconnect { reason })
        }
        PLAY_SET_HEALTH_ID => {
            let health = reader.read_f32()?;
            if !health.is_finite() {
                return Err(BootstrapProtocolError::NonFinite {
                    context: "Set Health value",
                });
            }
            let _food = reader.read_var_int()?;
            let _saturation = reader.read_f32()?;
            require_consumed(&reader, "Set Health")?;
            Ok(PlayClientbound::Health { health })
        }
        PLAY_SET_ENTITY_DATA_ID => {
            let entity_id = reader.read_var_int()?;
            let metadata = reader.read_remaining();
            let air_supply = decode_standalone_air_supply(metadata)?;
            Ok(PlayClientbound::EntityData {
                entity_id,
                air_supply,
                payload_bytes: metadata.len(),
            })
        }
        PLAY_START_CONFIGURATION_ID => {
            require_consumed(&reader, "Start Configuration")?;
            Ok(PlayClientbound::StartConfiguration)
        }
        PLAY_RESOURCE_PACK_PUSH_ID => Ok(PlayClientbound::ResourcePackPush),
        PLAY_TRANSFER_ID => Ok(PlayClientbound::Transfer),
        packet_id => Ok(PlayClientbound::Ignored {
            packet_id,
            payload_bytes: packet.payload.len(),
        }),
    }
}

fn decode_standalone_air_supply(metadata: &[u8]) -> Result<Option<i32>, BootstrapProtocolError> {
    let mut reader = CodecReader::new(metadata);
    if reader.remaining() == 0 || reader.read_u8()? != 1 {
        return Ok(None);
    }
    // EntityDataSerializers registers BYTE as 0 and INT as 1. Avoid trying to
    // skip arbitrary registry-dependent serializers: a packet containing a
    // different first item remains safely opaque.
    if reader.read_var_int()? != 1 {
        return Ok(None);
    }
    Ok(Some(reader.read_var_int()?))
}

/// Decodes 26.1.2's `LpVec3` representation. This replaced the historical
/// three-short entity-motion encoding and packs three normalized 15-bit
/// components plus a bounded integer scale.
fn decode_low_precision_vec3(
    reader: &mut CodecReader<'_>,
) -> Result<EntityMotion, BootstrapProtocolError> {
    let first = reader.read_u8()?;
    if first == 0 {
        return Ok(EntityMotion {
            entity_id: 0,
            delta_x: 0.0,
            delta_y: 0.0,
            delta_z: 0.0,
        });
    }
    let second = reader.read_u8()?;
    let upper = u64::from(reader.read_u32()?);
    let packed = (upper << 16) | (u64::from(second) << 8) | u64::from(first);
    let mut scale = u64::from(first & 0x03);
    if first & 0x04 != 0 {
        scale |= u64::from(reader.read_var_int()? as u32) << 2;
    }
    let unpack = |shift: u32| {
        let value = ((packed >> shift) & 0x7fff_u64).min(32_766) as f64;
        (value * 2.0 / 32_766.0 - 1.0) * scale as f64
    };
    Ok(EntityMotion {
        entity_id: 0,
        delta_x: unpack(3),
        delta_y: unpack(18),
        delta_z: unpack(33),
    })
}

fn decode_section_blocks_update(
    reader: &mut CodecReader<'_>,
) -> Result<SectionBlocksUpdate, BootstrapProtocolError> {
    let packed_section = reader.read_i64()?;
    let section_x =
        i32::try_from(packed_section >> 42).map_err(|_| CodecError::ValueOutOfRange {
            context: "Section Blocks Update section x",
            value: i128::from(packed_section >> 42),
            min: i128::from(i32::MIN),
            max: i128::from(i32::MAX),
        })?;
    let section_z =
        i32::try_from((packed_section << 22) >> 42).map_err(|_| CodecError::ValueOutOfRange {
            context: "Section Blocks Update section z",
            value: i128::from((packed_section << 22) >> 42),
            min: i128::from(i32::MIN),
            max: i128::from(i32::MAX),
        })?;
    let section_y =
        i32::try_from((packed_section << 44) >> 44).map_err(|_| CodecError::ValueOutOfRange {
            context: "Section Blocks Update section y",
            value: i128::from((packed_section << 44) >> 44),
            min: i128::from(i32::MIN),
            max: i128::from(i32::MAX),
        })?;
    let count = read_count(
        reader,
        "Section Blocks Update entries",
        MAX_SECTION_BLOCK_UPDATES,
    )?;
    let mut updates = Vec::new();
    updates
        .try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "Section Blocks Update entries",
            requested: count,
        })?;
    for _ in 0..count {
        let packed = reader.read_var_long()?;
        if packed < 0 {
            return Err(CodecError::ValueOutOfRange {
                context: "Section Blocks Update packed state",
                value: i128::from(packed),
                min: 0,
                max: i128::from(i64::MAX),
            }
            .into());
        }
        let packed = u64::try_from(packed).map_err(|_| CodecError::ValueOutOfRange {
            context: "Section Blocks Update packed state",
            value: i128::from(packed),
            min: 0,
            max: i128::from(i64::MAX),
        })?;
        let state_id = u32::try_from(packed >> 12).map_err(|_| CodecError::ValueOutOfRange {
            context: "Section Blocks Update state ID",
            value: i128::from(packed >> 12),
            min: 0,
            max: i128::from(u32::MAX),
        })?;
        updates.push(SectionBlockUpdate {
            local_x: u8::try_from((packed >> 8) & 0x0f).unwrap_or(0),
            local_y: u8::try_from(packed & 0x0f).unwrap_or(0),
            local_z: u8::try_from((packed >> 4) & 0x0f).unwrap_or(0),
            state_id,
        });
    }
    require_consumed(reader, "Section Blocks Update")?;
    Ok(SectionBlocksUpdate {
        section_x,
        section_y,
        section_z,
        updates,
    })
}

fn nonnegative_u32(value: i32, context: &'static str) -> Result<u32, BootstrapProtocolError> {
    u32::try_from(value).map_err(|_| {
        CodecError::ValueOutOfRange {
            context,
            value: i128::from(value),
            min: 0,
            max: i128::from(i32::MAX),
        }
        .into()
    })
}

fn decode_initial_play_login(
    reader: &mut CodecReader<'_>,
) -> Result<InitialPlayLogin, BootstrapProtocolError> {
    let player_entity_id = reader.read_i32()?;
    let hardcore = reader.read_bool()?;
    let dimension_count = read_count(reader, "known dimension", MAX_KNOWN_DIMENSIONS)?;
    let mut known_dimensions = Vec::new();
    known_dimensions
        .try_reserve(dimension_count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "known dimensions",
            requested: dimension_count,
        })?;
    for _ in 0..dimension_count {
        known_dimensions.push(reader.read_string(RESOURCE_LOCATION_LIMITS)?.to_owned());
    }
    let result = InitialPlayLogin {
        player_entity_id,
        hardcore,
        known_dimensions,
        max_players: reader.read_var_int()?,
        view_distance: reader.read_var_int()?,
        simulation_distance: reader.read_var_int()?,
        reduced_debug_info: reader.read_bool()?,
        show_death_screen: reader.read_bool()?,
        limited_crafting: reader.read_bool()?,
        spawn: decode_spawn_info(reader)?,
        secure_chat_enforced: reader.read_bool()?,
    };
    require_consumed(reader, "initial Play Login")?;
    Ok(result)
}

fn decode_spawn_info(reader: &mut CodecReader<'_>) -> Result<SpawnInfo, BootstrapProtocolError> {
    Ok(SpawnInfo {
        dimension_type_raw_id: reader.read_var_int()?,
        dimension: reader.read_string(RESOURCE_LOCATION_LIMITS)?.to_owned(),
        hashed_seed: reader.read_i64()?,
        game_mode: reader.read_i8()?,
        previous_game_mode: reader.read_u8()?,
        debug_world: reader.read_bool()?,
        flat_world: reader.read_bool()?,
        last_death_location: if reader.read_bool()? {
            Some(decode_global_position(reader)?)
        } else {
            None
        },
        portal_cooldown_ticks: reader.read_var_int()?,
        sea_level: reader.read_var_int()?,
    })
}

fn decode_global_position(
    reader: &mut CodecReader<'_>,
) -> Result<GlobalPosition, BootstrapProtocolError> {
    Ok(GlobalPosition {
        dimension: reader.read_string(RESOURCE_LOCATION_LIMITS)?.to_owned(),
        position: reader.read_block_position()?,
    })
}

fn decode_player_position(
    reader: &mut CodecReader<'_>,
) -> Result<PlayerPosition, BootstrapProtocolError> {
    let result = PlayerPosition {
        teleport_id: reader.read_var_int()?,
        x: reader.read_f64()?,
        y: reader.read_f64()?,
        z: reader.read_f64()?,
        delta_x: reader.read_f64()?,
        delta_y: reader.read_f64()?,
        delta_z: reader.read_f64()?,
        yaw: reader.read_f32()?,
        pitch: reader.read_f32()?,
        relative_flags: reader.read_u32()?,
    };
    if result.relative_flags & !0x01ff != 0 {
        return Err(CodecError::ValueOutOfRange {
            context: "Player Position relative flags",
            value: i128::from(result.relative_flags),
            min: 0,
            max: 0x01ff,
        }
        .into());
    }
    require_consumed(reader, "Play Player Position")?;
    Ok(result)
}

fn decode_respawn(reader: &mut CodecReader<'_>) -> Result<Respawn, BootstrapProtocolError> {
    let result = Respawn {
        spawn: decode_spawn_info(reader)?,
        data_to_keep: reader.read_u8()?,
    };
    require_consumed(reader, "Play Respawn")?;
    Ok(result)
}

fn decode_default_spawn_position(
    reader: &mut CodecReader<'_>,
) -> Result<DefaultSpawnPosition, BootstrapProtocolError> {
    let result = DefaultSpawnPosition {
        position: decode_global_position(reader)?,
        yaw: reader.read_f32()?,
        pitch: reader.read_f32()?,
    };
    require_consumed(reader, "Set Default Spawn Position")?;
    Ok(result)
}

fn decode_world_time(reader: &mut CodecReader<'_>) -> Result<WorldTime, BootstrapProtocolError> {
    let game_time = reader.read_i64()?;
    let count = read_count(reader, "world clock", MAX_WORLD_CLOCKS)?;
    let mut clocks = Vec::new();
    clocks
        .try_reserve(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "world clocks",
            requested: count,
        })?;
    for _ in 0..count {
        clocks.push(WorldClock {
            clock_type_raw_id: reader.read_var_int()?,
            total_ticks: reader.read_var_long()?,
            partial_tick: reader.read_f32()?,
            rate: reader.read_f32()?,
        });
    }
    require_consumed(reader, "Set Time")?;
    Ok(WorldTime { game_time, clocks })
}

fn decode_initialize_border(
    reader: &mut CodecReader<'_>,
) -> Result<InitializeBorder, BootstrapProtocolError> {
    let result = InitializeBorder {
        center_x: reader.read_f64()?,
        center_z: reader.read_f64()?,
        old_diameter: reader.read_f64()?,
        new_diameter: reader.read_f64()?,
        lerp_millis: reader.read_var_long()?,
        absolute_max_size: reader.read_var_int()?,
        warning_blocks: reader.read_var_int()?,
        warning_seconds: reader.read_var_int()?,
    };
    require_consumed(reader, "Initialize World Border")?;
    Ok(result)
}

pub fn encode_play_keep_alive(id: i64) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_KEEP_ALIVE_RESPONSE_ID);
    writer.write_i64(id);
    frame(writer)
}

pub fn encode_play_chat_acknowledgement(count: i32) -> Result<Vec<u8>, BootstrapProtocolError> {
    if count <= 0 {
        return Err(CodecError::ValueOutOfRange {
            context: "chat acknowledgement count",
            value: i128::from(count),
            min: 1,
            max: i128::from(i32::MAX),
        }
        .into());
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_CHAT_ACKNOWLEDGEMENT_ID);
    writer.write_var_int(count);
    frame(writer)
}

pub fn encode_play_pong(id: i32) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_PONG_ID);
    writer.write_i32(id);
    frame(writer)
}

pub fn encode_play_teleport_confirmation(
    teleport_id: i32,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_CONFIRM_TELEPORT_ID);
    writer.write_var_int(teleport_id);
    frame(writer)
}

fn movement_flags(on_ground: bool, horizontal_collision: bool) -> u8 {
    u8::from(on_ground) | (u8::from(horizontal_collision) << 1)
}

pub fn encode_play_move_position(
    x: f64,
    y: f64,
    z: f64,
    on_ground: bool,
    horizontal_collision: bool,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return Err(BootstrapProtocolError::NonFinite {
            context: "Move Player Pos",
        });
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_MOVE_PLAYER_POS_ID);
    writer.write_f64(x);
    writer.write_f64(y);
    writer.write_f64(z);
    writer.write_u8(movement_flags(on_ground, horizontal_collision));
    frame(writer)
}

#[allow(clippy::too_many_arguments)]
pub fn encode_play_move_position_rotation(
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
    on_ground: bool,
    horizontal_collision: bool,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if !x.is_finite() || !y.is_finite() || !z.is_finite() || !yaw.is_finite() || !pitch.is_finite()
    {
        return Err(BootstrapProtocolError::NonFinite {
            context: "Move Player Pos Rot",
        });
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_MOVE_PLAYER_POS_ROT_ID);
    writer.write_f64(x);
    writer.write_f64(y);
    writer.write_f64(z);
    writer.write_f32(yaw);
    writer.write_f32(pitch);
    writer.write_u8(movement_flags(on_ground, horizontal_collision));
    frame(writer)
}

pub fn encode_play_move_rotation(
    yaw: f32,
    pitch: f32,
    on_ground: bool,
    horizontal_collision: bool,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if !yaw.is_finite() || !pitch.is_finite() {
        return Err(BootstrapProtocolError::NonFinite {
            context: "Move Player Rot",
        });
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_MOVE_PLAYER_ROT_ID);
    writer.write_f32(yaw);
    writer.write_f32(pitch);
    writer.write_u8(movement_flags(on_ground, horizontal_collision));
    frame(writer)
}

pub fn encode_play_move_status(
    on_ground: bool,
    horizontal_collision: bool,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_MOVE_PLAYER_STATUS_ONLY_ID);
    writer.write_u8(movement_flags(on_ground, horizontal_collision));
    frame(writer)
}

pub fn encode_play_player_input(input: PlayerInput) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut flags = 0_u8;
    flags |= u8::from(input.forward);
    flags |= u8::from(input.backward) << 1;
    flags |= u8::from(input.left) << 2;
    flags |= u8::from(input.right) << 3;
    flags |= u8::from(input.jump) << 4;
    flags |= u8::from(input.sneak) << 5;
    flags |= u8::from(input.sprint) << 6;
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_PLAYER_INPUT_ID);
    writer.write_u8(flags);
    frame(writer)
}

/// Announces the client's resolved flying state. Protocol 775 carries only
/// bit 1 in this serverbound packet; capability and speeds remain
/// server-authoritative in the clientbound abilities packet.
pub fn encode_play_player_abilities(flying: bool) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_PLAYER_ABILITIES_ID);
    writer.write_u8(u8::from(flying) << 1);
    frame(writer)
}

pub fn encode_play_player_command(
    entity_id: i32,
    action: PlayerCommandAction,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let action = match action {
        PlayerCommandAction::StartSprinting => 1,
        PlayerCommandAction::StopSprinting => 2,
    };
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_PLAYER_COMMAND_ID);
    writer.write_var_int(entity_id);
    writer.write_var_int(action);
    writer.write_var_int(0);
    frame(writer)
}

pub fn encode_play_player_action(
    action: PlayerAction,
    position: crate::BlockPosition,
    face: BlockFace,
    sequence: i32,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if sequence < 0 {
        return Err(BootstrapProtocolError::NegativeCount {
            context: "Player Action sequence",
            value: sequence,
        });
    }
    let action = match action {
        PlayerAction::StartDestroyBlock => 0,
        PlayerAction::AbortDestroyBlock => 1,
        PlayerAction::StopDestroyBlock => 2,
    };
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_PLAYER_ACTION_ID);
    writer.write_var_int(action);
    writer.write_block_position(position.x(), position.y(), position.z())?;
    writer.write_u8(face as u8);
    writer.write_var_int(sequence);
    frame(writer)
}

pub fn encode_play_use_item_on(
    hand: InteractionHand,
    hit: BlockHit,
    sequence: i32,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if sequence < 0 {
        return Err(BootstrapProtocolError::NegativeCount {
            context: "Use Item On sequence",
            value: sequence,
        });
    }
    if !hit.location_x.is_finite() || !hit.location_y.is_finite() || !hit.location_z.is_finite() {
        return Err(BootstrapProtocolError::NonFinite {
            context: "Use Item On hit location",
        });
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_USE_ITEM_ON_ID);
    writer.write_var_int(match hand {
        InteractionHand::Main => 0,
        InteractionHand::Off => 1,
    });
    writer.write_block_position(hit.position.x(), hit.position.y(), hit.position.z())?;
    writer.write_var_int(i32::from(hit.face as u8));
    writer.write_f32(hit.location_x);
    writer.write_f32(hit.location_y);
    writer.write_f32(hit.location_z);
    writer.write_bool(hit.inside);
    writer.write_bool(hit.world_border_hit);
    writer.write_var_int(sequence);
    frame(writer)
}

pub fn encode_play_use_item(
    hand: InteractionHand,
    sequence: i32,
    yaw: f32,
    pitch: f32,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if sequence < 0 {
        return Err(BootstrapProtocolError::NegativeCount {
            context: "Use Item sequence",
            value: sequence,
        });
    }
    if !yaw.is_finite() || !pitch.is_finite() {
        return Err(BootstrapProtocolError::NonFinite {
            context: "Use Item rotation",
        });
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_USE_ITEM_ID);
    writer.write_var_int(match hand {
        InteractionHand::Main => 0,
        InteractionHand::Off => 1,
    });
    writer.write_var_int(sequence);
    writer.write_f32(yaw);
    writer.write_f32(pitch);
    frame(writer)
}

pub fn encode_play_chunk_batch_received(
    desired_chunks_per_tick: f32,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_CHUNK_BATCH_RECEIVED_ID);
    writer.write_f32(desired_chunks_per_tick);
    frame(writer)
}

/// Ends one protocol-775 client tick. The current packet has no payload.
pub fn encode_play_client_tick_end() -> Result<Vec<u8>, BootstrapProtocolError> {
    packet_without_payload(PLAY_CLIENT_TICK_END_ID)
}

pub fn encode_play_client_information(
    information: &ClientInformation<'_>,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_CLIENT_INFORMATION_ID);
    write_client_information_fields(&mut writer, information)?;
    frame(writer)
}

pub fn encode_play_player_loaded() -> Result<Vec<u8>, BootstrapProtocolError> {
    packet_without_payload(PLAY_PLAYER_LOADED_ID)
}

pub fn encode_play_cookie_response(key: &str) -> Result<Vec<u8>, BootstrapProtocolError> {
    encode_empty_cookie_response(PLAY_COOKIE_RESPONSE_ID, key)
}

pub fn encode_play_acknowledge_configuration() -> Result<Vec<u8>, BootstrapProtocolError> {
    packet_without_payload(PLAY_ACKNOWLEDGE_CONFIGURATION_ID)
}

pub fn encode_play_chat_message(
    message: &str,
    timestamp_millis: i64,
    salt: i64,
    signature: Option<MessageSignature>,
    last_seen: ChatLastSeenUpdate,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    if message.is_empty() {
        return Err(CodecError::ValueOutOfRange {
            context: "chat message UTF-16 length",
            value: 0,
            min: 1,
            max: MAX_CHAT_UTF16_UNITS as i128,
        }
        .into());
    }
    if message.chars().any(char::is_control) {
        return Err(CodecError::ValueOutOfRange {
            context: "chat message control character",
            value: 1,
            min: 0,
            max: 0,
        }
        .into());
    }
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_CHAT_MESSAGE_ID);
    writer.write_string(message, CHAT_LIMITS)?;
    writer.write_i64(timestamp_millis);
    writer.write_i64(salt);
    writer.write_bool(signature.is_some());
    if let Some(signature) = signature {
        writer.write_bytes(signature.as_bytes());
    }
    writer.write_var_int(last_seen.offset);
    writer.write_bytes(&last_seen.acknowledged);
    writer.write_u8(last_seen.checksum);
    frame(writer)
}

/// Encodes protocol 775's Play-state player chat-session update (ID `0x0a`).
pub fn encode_play_chat_session_update(
    session_id: ProtocolUuid,
    expires_at_millis: i64,
    public_key_der: &[u8],
    public_key_signature: &[u8],
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(PLAY_CHAT_SESSION_UPDATE_ID);
    writer.write_uuid(session_id);
    writer.write_i64(expires_at_millis);
    writer.write_byte_array(public_key_der, MAX_PLAYER_PUBLIC_KEY_BYTES)?;
    writer.write_byte_array(public_key_signature, MAX_PLAYER_KEY_SIGNATURE_BYTES)?;
    frame(writer)
}

fn decode_player_chat(
    reader: &mut CodecReader<'_>,
) -> Result<PlayClientbound, BootstrapProtocolError> {
    let global_index = reader.read_var_int()?;
    let sender_uuid = reader.read_uuid()?;
    let sender_index = reader.read_var_int()?;
    let signature = if reader.read_bool()? {
        let bytes = reader.read_bytes(PLAYER_CHAT_SIGNATURE_BYTES, "Player Chat signature")?;
        Some(MessageSignature::new(bytes.try_into().map_err(|_| {
            CodecError::ValueOutOfRange {
                context: "Player Chat signature length",
                value: bytes.len() as i128,
                min: PLAYER_CHAT_SIGNATURE_BYTES as i128,
                max: PLAYER_CHAT_SIGNATURE_BYTES as i128,
            }
        })?))
    } else {
        None
    };
    let content = reader.read_string(CHAT_LIMITS)?.to_owned();
    let _timestamp = reader.read_i64()?;
    let _salt = reader.read_i64()?;
    let last_seen = read_count(
        reader,
        "Player Chat last-seen messages",
        MAX_LAST_SEEN_MESSAGES,
    )?;
    for _ in 0..last_seen {
        let cached_id = reader.read_var_int()?;
        if cached_id == 0 {
            let _signature = reader.read_bytes(256, "Player Chat last-seen signature")?;
        } else if cached_id < 0 {
            return Err(BootstrapProtocolError::NegativeCount {
                context: "Player Chat cached signature ID",
                value: cached_id,
            });
        }
    }
    let unsigned = if reader.read_bool()? {
        Some(decode_text_component(reader)?)
    } else {
        None
    };
    match reader.read_var_int()? {
        0 | 1 => {}
        2 => {
            let _mask = reader.read_bitset(BitSetLimits::new(4, MAX_CHAT_UTF16_UNITS))?;
        }
        value => {
            return Err(CodecError::ValueOutOfRange {
                context: "Player Chat filter mask type",
                value: i128::from(value),
                min: 0,
                max: 2,
            }
            .into());
        }
    }
    let sender_name = decode_bound_chat_type(reader)?;
    require_consumed(reader, "Player Chat")?;
    let modified = unsigned.is_some();
    let message = unsigned.clone().unwrap_or(TextComponent {
        value: NbtTag::String(crate::nbt::NbtString::from_utf16_units(
            content.encode_utf16().collect(),
        )),
        plain_text: content.clone(),
    });
    Ok(PlayClientbound::PlayerChat {
        sender_uuid,
        sender_name,
        signed_content: content,
        unsigned_content: unsigned,
        message,
        global_index,
        sender_index,
        signature,
        modified,
    })
}

fn decode_bound_chat_type(reader: &mut CodecReader<'_>) -> Result<String, BootstrapProtocolError> {
    let holder = reader.read_var_int()?;
    if holder < 0 {
        return Err(BootstrapProtocolError::NegativeCount {
            context: "Bound Chat Type holder",
            value: holder,
        });
    }
    if holder == 0 {
        let _inline = decode_text_component(reader)?;
    }
    let sender = decode_text_component(reader)?;
    if reader.read_bool()? {
        let _target = decode_text_component(reader)?;
    }
    Ok(sender.plain_text)
}

fn decode_text_component(
    reader: &mut CodecReader<'_>,
) -> Result<TextComponent, BootstrapProtocolError> {
    if reader.remaining() > MAX_CHAT_COMPONENT_BYTES {
        return Err(BootstrapProtocolError::PayloadTooLarge {
            context: "text component",
            length: reader.remaining(),
            max: MAX_CHAT_COMPONENT_BYTES,
        });
    }
    let value = decode_unnamed_network_tag(reader, NbtLimits::default())
        .map_err(BootstrapProtocolError::Nbt)?;
    let mut plain_text = String::new();
    project_plain_text(&value, &mut plain_text, 0);
    if plain_text.is_empty() {
        plain_text.push_str("<rich text>");
    }
    Ok(TextComponent { value, plain_text })
}

fn project_plain_text(value: &NbtTag, output: &mut String, depth: usize) {
    if depth > 32 || output.len() >= MAX_CHAT_COMPONENT_BYTES {
        return;
    }
    match value {
        NbtTag::String(value) => output.push_str(&value.to_string_lossy()),
        NbtTag::List(list) => {
            for child in list.elements() {
                project_plain_text(child, output, depth + 1);
            }
        }
        NbtTag::Compound(compound) => {
            if let Some(text) = compound.get_string("text") {
                output.push_str(&text.to_string_lossy());
            } else if let Some(translate) = compound.get_string("translate") {
                output.push_str(&translate.to_string_lossy());
                if let Some(NbtTag::List(arguments)) = compound.get_str("with") {
                    output.push(' ');
                    for argument in arguments.elements() {
                        project_plain_text(argument, output, depth + 1);
                    }
                }
            }
            if let Some(NbtTag::List(extra)) = compound.get_str("extra") {
                for child in extra.elements() {
                    project_plain_text(child, output, depth + 1);
                }
            }
        }
        _ => {}
    }
    if output.len() > MAX_CHAT_COMPONENT_BYTES {
        output.truncate(MAX_CHAT_COMPONENT_BYTES);
    }
}

fn decode_login_success<'a>(
    reader: &mut CodecReader<'a>,
) -> Result<LoginSuccess<'a>, BootstrapProtocolError> {
    let uuid = reader.read_uuid()?;
    let username = reader.read_string(USERNAME_LIMITS)?;
    let count = read_count(reader, "Game Profile properties", MAX_LOGIN_PROPERTIES)?;
    let mut properties = Vec::new();
    properties
        .try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "Game Profile properties",
            requested: count,
        })?;
    for _ in 0..count {
        let name = reader.read_string(PROPERTY_NAME_LIMITS)?;
        let value = reader.read_string(PROPERTY_VALUE_LIMITS)?;
        let signature = if reader.read_bool()? {
            Some(reader.read_string(PROPERTY_VALUE_LIMITS)?)
        } else {
            None
        };
        properties.push(GameProfileProperty {
            name,
            value,
            signature,
        });
    }
    require_consumed(reader, "Login Success")?;
    Ok(LoginSuccess {
        uuid,
        username,
        properties,
    })
}

fn decode_configuration_disconnect(
    payload: &[u8],
) -> Result<ConfigurationClientbound<'_>, BootstrapProtocolError> {
    check_payload("Configuration Disconnect", payload, MAX_DISCONNECT_BYTES)?;
    let limits = NbtLimits::default()
        .with_max_depth(16)
        .with_max_total_tags(1024)
        .with_max_compound_entries(256)
        .with_max_list_elements(1024)
        .with_max_array_elements(4096)
        .with_max_string_encoded_bytes(4096)
        .with_max_total_allocated_bytes(MAX_DISCONNECT_BYTES);
    let reason = decode_unnamed_network_root_complete(payload, limits)
        .map_err(BootstrapProtocolError::Nbt)?;
    Ok(ConfigurationClientbound::Disconnect { reason })
}

fn decode_known_packs<'a>(
    reader: &mut CodecReader<'a>,
) -> Result<ConfigurationClientbound<'a>, BootstrapProtocolError> {
    let count = read_count(reader, "Known Packs", MAX_KNOWN_PACKS)?;
    let mut packs = Vec::new();
    packs
        .try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "Known Packs",
            requested: count,
        })?;
    for _ in 0..count {
        packs.push(KnownPack {
            namespace: reader.read_string(KNOWN_PACK_NAMESPACE_LIMITS)?,
            id: reader.read_string(KNOWN_PACK_ID_LIMITS)?,
            version: reader.read_string(KNOWN_PACK_VERSION_LIMITS)?,
        });
    }
    require_consumed(reader, "Select Known Packs")?;
    Ok(ConfigurationClientbound::KnownPacks(packs))
}

fn decode_configuration_registry_data<'a>(
    reader: &mut CodecReader<'a>,
) -> Result<ConfigurationClientbound<'a>, BootstrapProtocolError> {
    let registry = reader.read_string(IDENTIFIER_LIMITS)?;
    let count = read_count(
        reader,
        "Configuration Registry Data",
        MAX_CONFIGURATION_REGISTRY_ENTRIES,
    )?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| CodecError::AllocationFailed {
            context: "Configuration Registry Data",
            requested: count,
        })?;
    let limits = NbtLimits::default()
        .with_max_total_tags(65_536)
        .with_max_total_allocated_bytes(MAX_CONFIGURATION_BUFFERED_BYTES);
    for _ in 0..count {
        let identifier = reader.read_string(IDENTIFIER_LIMITS)?;
        let data = if reader.read_bool()? {
            Some(decode_unnamed_network_tag(reader, limits).map_err(BootstrapProtocolError::Nbt)?)
        } else {
            None
        };
        entries.push(ConfigurationRegistryEntry { identifier, data });
    }
    require_consumed(reader, "Configuration Registry Data")?;
    Ok(ConfigurationClientbound::RegistryData { registry, entries })
}

fn read_count(
    reader: &mut CodecReader<'_>,
    context: &'static str,
    max: usize,
) -> Result<usize, BootstrapProtocolError> {
    let value = reader.read_var_int()?;
    if value < 0 {
        return Err(BootstrapProtocolError::NegativeCount { context, value });
    }
    let count = usize::try_from(value).map_err(|_| BootstrapProtocolError::CountTooLarge {
        context,
        count: usize::MAX,
        max,
    })?;
    if count > max {
        Err(BootstrapProtocolError::CountTooLarge {
            context,
            count,
            max,
        })
    } else {
        Ok(count)
    }
}

fn encode_empty_cookie_response(
    packet_id: i32,
    key: &str,
) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(packet_id);
    writer.write_string(key, IDENTIFIER_LIMITS)?;
    writer.write_bool(false);
    frame(writer)
}

fn packet_without_payload(packet_id: i32) -> Result<Vec<u8>, BootstrapProtocolError> {
    let mut writer = CodecWriter::new();
    writer.write_var_int(packet_id);
    frame(writer)
}

fn frame(writer: CodecWriter) -> Result<Vec<u8>, BootstrapProtocolError> {
    Ok(encode_frame(writer.as_slice(), MAX_BOOTSTRAP_FRAME_SIZE)?)
}

fn require_consumed(
    reader: &CodecReader<'_>,
    context: &'static str,
) -> Result<(), BootstrapProtocolError> {
    if reader.remaining() == 0 {
        Ok(())
    } else {
        Err(BootstrapProtocolError::TrailingData {
            context,
            remaining: reader.remaining(),
        })
    }
}

fn check_payload(
    context: &'static str,
    payload: &[u8],
    max: usize,
) -> Result<(), BootstrapProtocolError> {
    if payload.len() > max {
        Err(BootstrapProtocolError::PayloadTooLarge {
            context,
            length: payload.len(),
            max,
        })
    } else {
        Ok(())
    }
}

fn skipped(
    packet: SkippedConfigurationPacket,
    payload_bytes: usize,
) -> ConfigurationClientbound<'static> {
    ConfigurationClientbound::Skipped {
        packet,
        payload_bytes,
    }
}

fn decode_empty_skipped(
    reader: &CodecReader<'_>,
    packet: SkippedConfigurationPacket,
    context: &'static str,
) -> Result<ConfigurationClientbound<'static>, BootstrapProtocolError> {
    require_consumed(reader, context)?;
    Ok(skipped(packet, 0))
}
