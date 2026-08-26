use std::{fmt, net::Ipv6Addr, str::FromStr};

use thiserror::Error;

pub const DEFAULT_MINECRAFT_PORT: u16 = 25_565;
const MAX_LOGICAL_HOST_UTF16_UNITS: usize = 255;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServerAddress {
    host: String,
    port: u16,
    ipv6_literal: bool,
}

impl ServerAddress {
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub const fn is_ipv6_literal(&self) -> bool {
        self.ipv6_literal
    }

    pub(crate) fn socket_target(&self) -> (&str, u16) {
        (&self.host, self.port)
    }
}

impl FromStr for ServerAddress {
    type Err = ServerAddressError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(ServerAddressError::EmptyHost);
        }
        if input.chars().any(char::is_whitespace) {
            return Err(ServerAddressError::Whitespace);
        }

        if let Some(bracketed) = input.strip_prefix('[') {
            return parse_bracketed_ipv6(bracketed);
        }
        if input.contains(['[', ']']) {
            return Err(ServerAddressError::MalformedBrackets);
        }

        let colon_count = input.bytes().filter(|byte| *byte == b':').count();
        let (host, port) = match colon_count {
            0 => (input, DEFAULT_MINECRAFT_PORT),
            1 => {
                let (host, port) = input
                    .split_once(':')
                    .ok_or(ServerAddressError::InvalidSyntax)?;
                (host, parse_port(port)?)
            }
            _ => return Err(ServerAddressError::UnbracketedIpv6),
        };
        validate_host(host)?;
        Ok(Self {
            host: host.to_owned(),
            port,
            ipv6_literal: false,
        })
    }
}

impl fmt::Display for ServerAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ipv6_literal {
            write!(formatter, "[{}]:{}", self.host, self.port)
        } else {
            write!(formatter, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ServerAddressError {
    #[error("server host is empty")]
    EmptyHost,
    #[error("server address must not contain whitespace")]
    Whitespace,
    #[error("server address has malformed bracket syntax")]
    MalformedBrackets,
    #[error("IPv6 literals must use bracket syntax such as [::1]:25565")]
    UnbracketedIpv6,
    #[error("server address syntax is invalid")]
    InvalidSyntax,
    #[error("server port is missing")]
    MissingPort,
    #[error("server port is not a valid unsigned 16-bit integer")]
    InvalidPort,
    #[error("server port zero is not permitted")]
    ZeroPort,
    #[error("bracketed host is not a valid IPv6 literal")]
    InvalidIpv6,
    #[error("server host exceeds {max} Java UTF-16 code units")]
    HostTooLong { max: usize },
    #[error("server host contains a control character")]
    ControlCharacter,
}

fn parse_bracketed_ipv6(input_after_open: &str) -> Result<ServerAddress, ServerAddressError> {
    let close = input_after_open
        .find(']')
        .ok_or(ServerAddressError::MalformedBrackets)?;
    let host = input_after_open
        .get(..close)
        .ok_or(ServerAddressError::MalformedBrackets)?;
    if host.is_empty() {
        return Err(ServerAddressError::EmptyHost);
    }
    host.parse::<Ipv6Addr>()
        .map_err(|_| ServerAddressError::InvalidIpv6)?;
    let suffix = input_after_open
        .get(close + 1..)
        .ok_or(ServerAddressError::MalformedBrackets)?;
    let port = if suffix.is_empty() {
        DEFAULT_MINECRAFT_PORT
    } else {
        let port = suffix
            .strip_prefix(':')
            .ok_or(ServerAddressError::MalformedBrackets)?;
        parse_port(port)?
    };
    Ok(ServerAddress {
        host: host.to_owned(),
        port,
        ipv6_literal: true,
    })
}

fn validate_host(host: &str) -> Result<(), ServerAddressError> {
    if host.is_empty() {
        return Err(ServerAddressError::EmptyHost);
    }
    if host.encode_utf16().count() > MAX_LOGICAL_HOST_UTF16_UNITS {
        return Err(ServerAddressError::HostTooLong {
            max: MAX_LOGICAL_HOST_UTF16_UNITS,
        });
    }
    if host.chars().any(char::is_control) {
        return Err(ServerAddressError::ControlCharacter);
    }
    Ok(())
}

fn parse_port(value: &str) -> Result<u16, ServerAddressError> {
    if value.is_empty() {
        return Err(ServerAddressError::MissingPort);
    }
    let port = value
        .parse::<u16>()
        .map_err(|_| ServerAddressError::InvalidPort)?;
    if port == 0 {
        Err(ServerAddressError::ZeroPort)
    } else {
        Ok(port)
    }
}
