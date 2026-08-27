use std::io::{Read, Write};

use aes::{Aes128, cipher::KeyIvInit};
use cfb8::{Decryptor, Encryptor};
use cubic_protocol::{CodecReader, CodecWriter, FrameDecoder, FrameLimits, encode_frame};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use thiserror::Error;

type AesCfb8Encryptor = Encryptor<Aes128>;
type AesCfb8Decryptor = Decryptor<Aes128>;

#[derive(Debug, Error)]
pub(crate) enum TransformError {
    #[error("invalid AES-128 shared secret")]
    InvalidSecret,
    #[error("malformed framed packet supplied to transport")]
    InvalidOutboundFrame,
    #[error("invalid packet compression threshold {0}")]
    InvalidCompressionThreshold(i32),
    #[error("compressed packet declared a negative size {0}")]
    NegativeDecompressedLength(i32),
    #[error("compressed packet declared {declared} bytes, exceeding limit {max}")]
    DecompressedTooLarge { declared: usize, max: usize },
    #[error("compressed packet declared {declared} bytes but produced {actual}")]
    DecompressedLengthMismatch { declared: usize, actual: usize },
    #[error("compressed packet of {declared} bytes is below negotiated threshold {threshold}")]
    CompressedBelowThreshold { declared: usize, threshold: usize },
    #[error("zlib packet compression failed")]
    Compression(#[source] std::io::Error),
    #[error("packet framing failed")]
    Framing(#[source] cubic_protocol::CodecError),
}

pub(crate) struct WireTransforms {
    encryptor: Option<AesCfb8Encryptor>,
    decryptor: Option<AesCfb8Decryptor>,
    compression_threshold: Option<usize>,
    frame_limits: FrameLimits,
}

impl WireTransforms {
    pub(crate) const fn new(frame_limits: FrameLimits) -> Self {
        Self {
            encryptor: None,
            decryptor: None,
            compression_threshold: None,
            frame_limits,
        }
    }

    pub(crate) fn enable_encryption(&mut self, secret: &[u8; 16]) -> Result<(), TransformError> {
        self.encryptor = Some(
            AesCfb8Encryptor::new_from_slices(secret, secret)
                .map_err(|_| TransformError::InvalidSecret)?,
        );
        self.decryptor = Some(
            AesCfb8Decryptor::new_from_slices(secret, secret)
                .map_err(|_| TransformError::InvalidSecret)?,
        );
        Ok(())
    }

    pub(crate) fn enable_compression(&mut self, threshold: i32) -> Result<(), TransformError> {
        let threshold = usize::try_from(threshold)
            .map_err(|_| TransformError::InvalidCompressionThreshold(threshold))?;
        self.compression_threshold = Some(threshold);
        Ok(())
    }

    pub(crate) fn decrypt_in_place(&mut self, bytes: &mut [u8]) {
        if let Some(decryptor) = &mut self.decryptor {
            decryptor.decrypt(bytes);
        }
    }

    pub(crate) fn encode_outbound(&mut self, framed: &[u8]) -> Result<Vec<u8>, TransformError> {
        let body = extract_single_frame(framed, self.frame_limits)?;
        let mut output = if let Some(threshold) = self.compression_threshold {
            encode_compressed(&body, threshold, self.frame_limits.max_frame_size())?
        } else {
            framed.to_vec()
        };
        if let Some(encryptor) = &mut self.encryptor {
            encryptor.encrypt(&mut output);
        }
        Ok(output)
    }

    pub(crate) fn decode_frame_body(&self, body: Vec<u8>) -> Result<Vec<u8>, TransformError> {
        let Some(threshold) = self.compression_threshold else {
            return Ok(body);
        };
        decode_compressed(&body, threshold, self.frame_limits.max_frame_size())
    }
}

fn extract_single_frame(bytes: &[u8], limits: FrameLimits) -> Result<Vec<u8>, TransformError> {
    let mut decoder = FrameDecoder::new(limits);
    decoder.push(bytes).map_err(TransformError::Framing)?;
    let frame = decoder
        .next_frame()
        .map_err(TransformError::Framing)?
        .ok_or(TransformError::InvalidOutboundFrame)?;
    if decoder.buffered_len() != 0 {
        return Err(TransformError::InvalidOutboundFrame);
    }
    Ok(frame)
}

fn encode_compressed(body: &[u8], threshold: usize, max: usize) -> Result<Vec<u8>, TransformError> {
    let mut payload = CodecWriter::new();
    if body.len() >= threshold {
        let declared =
            i32::try_from(body.len()).map_err(|_| TransformError::DecompressedTooLarge {
                declared: body.len(),
                max,
            })?;
        payload.write_var_int(declared);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(body)
            .map_err(TransformError::Compression)?;
        let compressed = encoder.finish().map_err(TransformError::Compression)?;
        payload.write_bytes(&compressed);
    } else {
        payload.write_var_int(0);
        payload.write_bytes(body);
    }
    encode_frame(payload.as_slice(), max).map_err(TransformError::Framing)
}

fn decode_compressed(
    payload: &[u8],
    threshold: usize,
    max: usize,
) -> Result<Vec<u8>, TransformError> {
    let mut reader = CodecReader::new(payload);
    let declared = reader.read_var_int().map_err(TransformError::Framing)?;
    if declared < 0 {
        return Err(TransformError::NegativeDecompressedLength(declared));
    }
    let compressed = reader.read_remaining();
    if declared == 0 {
        if compressed.len() >= threshold {
            return Err(TransformError::DecompressedLengthMismatch {
                declared: 0,
                actual: compressed.len(),
            });
        }
        return Ok(compressed.to_vec());
    }
    let declared = usize::try_from(declared).map_err(|_| TransformError::DecompressedTooLarge {
        declared: usize::MAX,
        max,
    })?;
    if declared > max {
        return Err(TransformError::DecompressedTooLarge { declared, max });
    }
    if declared < threshold {
        return Err(TransformError::CompressedBelowThreshold {
            declared,
            threshold,
        });
    }
    let limit = u64::try_from(max.saturating_add(1)).unwrap_or(u64::MAX);
    let mut decoder = ZlibDecoder::new(compressed).take(limit);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(TransformError::Compression)?;
    if output.len() != declared {
        return Err(TransformError::DecompressedLengthMismatch {
            declared,
            actual: output.len(),
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use cubic_protocol::{FrameLimits, encode_frame};

    use super::WireTransforms;

    fn limits() -> FrameLimits {
        FrameLimits::new(1024, 2048).unwrap()
    }

    #[test]
    fn compression_transition_handles_below_and_at_threshold() {
        let mut transforms = WireTransforms::new(limits());
        transforms.enable_compression(8).unwrap();
        for body in [vec![1, 2, 3], vec![7; 8]] {
            let framed = encode_frame(&body, 1024).unwrap();
            let wire = transforms.encode_outbound(&framed).unwrap();
            let outer = super::extract_single_frame(&wire, limits()).unwrap();
            assert_eq!(transforms.decode_frame_body(outer).unwrap(), body);
        }
    }

    #[test]
    fn cfb8_state_is_continuous_across_fragmentation() {
        let secret = [0x42; 16];
        let mut sender = WireTransforms::new(limits());
        let mut receiver = WireTransforms::new(limits());
        sender.enable_encryption(&secret).unwrap();
        receiver.enable_encryption(&secret).unwrap();
        let first = sender
            .encode_outbound(&encode_frame(b"first", 1024).unwrap())
            .unwrap();
        let second = sender
            .encode_outbound(&encode_frame(b"second", 1024).unwrap())
            .unwrap();
        let mut joined = [first, second].concat();
        for chunk in joined.chunks_mut(3) {
            receiver.decrypt_in_place(chunk);
        }
        let mut decoder = cubic_protocol::FrameDecoder::new(limits());
        decoder.push(&joined).unwrap();
        assert_eq!(decoder.next_frame().unwrap().unwrap(), b"first");
        assert_eq!(decoder.next_frame().unwrap().unwrap(), b"second");
    }

    #[test]
    fn declared_size_mismatch_and_bombs_are_rejected() {
        let mut transforms = WireTransforms::new(limits());
        transforms.enable_compression(1).unwrap();
        assert!(super::decode_compressed(&[0x80, 0x08], 1, 1024).is_err());
        let mut malformed = cubic_protocol::CodecWriter::new();
        malformed.write_var_int(4);
        malformed.write_bytes(&[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert!(super::decode_compressed(malformed.as_slice(), 1, 1024).is_err());
    }
}
