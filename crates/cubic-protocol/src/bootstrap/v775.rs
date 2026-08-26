//! Narrow Minecraft Java 26.1.2 / protocol 775 Login and Configuration profile.
//!
//! This module is temporary Phase 7 bootstrap data. Phase 12 will replace its
//! manually maintained packet IDs and shapes with generated packet codecs.

use thiserror::Error;

use crate::{
    CodecError, CodecReader, CodecWriter, MINECRAFT_MAX_FRAME_SIZE, ProtocolUuid, StringLimits,
    encode_frame,
    nbt::{NbtCompound, NbtError, NbtLimits, decode_unnamed_network_root_complete},
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
pub const MAX_DISCONNECT_BYTES: usize = 32 * 1024;
pub const MAX_KNOWN_PACKS: usize = 64;

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

const USERNAME_LIMITS: StringLimits = StringLimits::new(MAX_USERNAME_UTF16_UNITS, 48);
const IDENTIFIER_LIMITS: StringLimits = StringLimits::new(32_767, 32_767);
const DISCONNECT_LIMITS: StringLimits =
    StringLimits::new(MAX_DISCONNECT_BYTES, MAX_DISCONNECT_BYTES);
const PROPERTY_NAME_LIMITS: StringLimits = StringLimits::new(64, 192);
const PROPERTY_VALUE_LIMITS: StringLimits = StringLimits::new(16_384, 49_152);
const KNOWN_PACK_NAMESPACE_LIMITS: StringLimits = StringLimits::new(64, 192);
const KNOWN_PACK_ID_LIMITS: StringLimits = StringLimits::new(256, 768);
const KNOWN_PACK_VERSION_LIMITS: StringLimits = StringLimits::new(128, 384);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginSuccess<'a> {
    pub uuid: ProtocolUuid,
    pub username: &'a str,
    pub properties: Vec<GameProfileProperty<'a>>,
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
    EncryptionRequest,
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
    #[error("initial Play Login packet has no payload")]
    EmptyInitialPlayLogin,
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
        LOGIN_ENCRYPTION_REQUEST_ID => Ok(LoginClientbound::EncryptionRequest),
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
    writer.write_string(information.locale, StringLimits::new(16, 48))?;
    writer.write_i8(information.view_distance);
    writer.write_var_int(information.chat_mode);
    writer.write_bool(information.chat_colors);
    writer.write_u8(information.displayed_skin_parts);
    writer.write_var_int(information.main_hand);
    writer.write_bool(information.text_filtering);
    writer.write_bool(information.allows_server_listing);
    writer.write_var_int(information.particle_status);
    frame(writer)
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
        CONFIG_REGISTRY_DATA_ID => Ok(skipped(
            SkippedConfigurationPacket::RegistryData,
            packet.payload.len(),
        )),
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

pub fn validate_initial_play_login(frame_body: &[u8]) -> Result<(), BootstrapProtocolError> {
    let packet = split_raw_packet(frame_body)?;
    if packet.id != INITIAL_PLAY_LOGIN_ID {
        return Err(BootstrapProtocolError::UnexpectedPacketId {
            state: "Play acceptance",
            id: packet.id,
        });
    }
    if packet.payload.is_empty() {
        return Err(BootstrapProtocolError::EmptyInitialPlayLogin);
    }
    Ok(())
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
