use crate::{CodecError, CodecWriter, StringLimits, encode_frame};

pub const HANDSHAKE_PACKET_ID: i32 = 0;
pub const MAX_HANDSHAKE_HOST_UTF16_UNITS: usize = 255;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum HandshakeNextState {
    Status = 1,
    Login = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Handshake<'a> {
    pub protocol_version: i32,
    pub server_address: &'a str,
    pub server_port: u16,
    pub next_state: HandshakeNextState,
}

pub fn encode_handshake(
    handshake: &Handshake<'_>,
    max_frame_size: usize,
) -> Result<Vec<u8>, CodecError> {
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
    writer.write_var_int(handshake.next_state as i32);
    encode_frame(writer.as_slice(), max_frame_size)
}
