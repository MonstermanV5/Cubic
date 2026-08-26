use std::{io, time::Duration};

use cubic_protocol::{CodecError, FrameDecoder, FrameLimits};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

use crate::ServerAddress;

const READ_BUFFER_SIZE: usize = 8 * 1024;

#[derive(Debug, Error)]
pub(crate) enum ConnectionError {
    #[error("timed out connecting after {timeout:?}")]
    ConnectTimeout { timeout: Duration },
    #[error("could not resolve or connect to the server")]
    ConnectFailed {
        #[source]
        source: io::Error,
    },
    #[error("timed out during {operation} after {timeout:?}")]
    IoTimeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("I/O failure during {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("server disconnected before {phase}; {buffered_bytes} partial bytes were buffered")]
    PrematureDisconnect {
        phase: &'static str,
        buffered_bytes: usize,
    },
    #[error("malformed Minecraft frame")]
    Framing(#[source] CodecError),
}

pub(crate) struct MinecraftConnection {
    stream: TcpStream,
    decoder: FrameDecoder,
    io_timeout: Duration,
}

impl MinecraftConnection {
    pub(crate) async fn connect(
        address: &ServerAddress,
        connect_timeout: Duration,
        io_timeout: Duration,
        limits: FrameLimits,
    ) -> Result<Self, ConnectionError> {
        if connect_timeout.is_zero() {
            return Err(ConnectionError::ConnectTimeout {
                timeout: connect_timeout,
            });
        }
        let stream =
            match timeout(connect_timeout, TcpStream::connect(address.socket_target())).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(source)) => return Err(ConnectionError::ConnectFailed { source }),
                Err(_) => {
                    return Err(ConnectionError::ConnectTimeout {
                        timeout: connect_timeout,
                    });
                }
            };
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(limits),
            io_timeout,
        })
    }

    pub(crate) async fn write_all(
        &mut self,
        bytes: &[u8],
        operation: &'static str,
    ) -> Result<(), ConnectionError> {
        match timeout(self.io_timeout, self.stream.write_all(bytes)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(source)) => Err(ConnectionError::Io { operation, source }),
            Err(_) => Err(ConnectionError::IoTimeout {
                operation,
                timeout: self.io_timeout,
            }),
        }
    }

    pub(crate) async fn read_frame(
        &mut self,
        phase: &'static str,
    ) -> Result<Vec<u8>, ConnectionError> {
        match timeout(self.io_timeout, self.read_next_frame(phase)).await {
            Ok(result) => result,
            Err(_) => Err(ConnectionError::IoTimeout {
                operation: phase,
                timeout: self.io_timeout,
            }),
        }
    }

    async fn read_next_frame(&mut self, phase: &'static str) -> Result<Vec<u8>, ConnectionError> {
        let mut read_buffer = [0_u8; READ_BUFFER_SIZE];
        loop {
            match self.decoder.next_frame() {
                Ok(Some(frame)) => return Ok(frame),
                Ok(None) => {}
                Err(error) => return Err(ConnectionError::Framing(error)),
            }

            let read =
                self.stream
                    .read(&mut read_buffer)
                    .await
                    .map_err(|source| ConnectionError::Io {
                        operation: phase,
                        source,
                    })?;
            if read == 0 {
                return Err(ConnectionError::PrematureDisconnect {
                    phase,
                    buffered_bytes: self.decoder.buffered_len(),
                });
            }
            let bytes = read_buffer
                .get(..read)
                .ok_or(ConnectionError::PrematureDisconnect {
                    phase,
                    buffered_bytes: self.decoder.buffered_len(),
                })?;
            self.decoder.push(bytes).map_err(ConnectionError::Framing)?;
        }
    }
}
