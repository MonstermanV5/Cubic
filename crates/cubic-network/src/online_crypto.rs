use rand_core_06::{OsRng, RngCore};
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs8::DecodePublicKey, traits::PublicKeyParts};
use sha1::{Digest, Sha1};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OnlineCryptoError {
    #[error("server supplied an invalid RSA SubjectPublicKeyInfo key")]
    InvalidPublicKey,
    #[error("server RSA key size {bits} is outside the accepted 1024..=4096-bit range")]
    InvalidRsaKeySize { bits: usize },
    #[error("RSA PKCS#1 v1.5 encryption failed")]
    RsaEncryption,
}

pub(crate) struct EncryptionMaterial {
    pub(crate) shared_secret: [u8; 16],
    pub(crate) encrypted_secret: Vec<u8>,
    pub(crate) encrypted_verify_token: Vec<u8>,
}

pub(crate) fn prepare_encryption(
    public_key_der: &[u8],
    verify_token: &[u8],
) -> Result<EncryptionMaterial, OnlineCryptoError> {
    let key = RsaPublicKey::from_public_key_der(public_key_der)
        .map_err(|_| OnlineCryptoError::InvalidPublicKey)?;
    let bits = key.n().bits();
    if !(1024..=4096).contains(&bits) {
        return Err(OnlineCryptoError::InvalidRsaKeySize { bits });
    }
    let mut shared_secret = [0_u8; 16];
    OsRng.fill_bytes(&mut shared_secret);
    let encrypted_secret = key
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, &shared_secret)
        .map_err(|_| OnlineCryptoError::RsaEncryption)?;
    let encrypted_verify_token = key
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, verify_token)
        .map_err(|_| OnlineCryptoError::RsaEncryption)?;
    Ok(EncryptionMaterial {
        shared_secret,
        encrypted_secret,
        encrypted_verify_token,
    })
}

#[must_use]
pub fn minecraft_server_hash(
    server_id: &str,
    shared_secret: &[u8],
    public_key_der: &[u8],
) -> String {
    let digest = Sha1::new()
        .chain_update(server_id.as_bytes())
        .chain_update(shared_secret)
        .chain_update(public_key_der)
        .finalize();
    signed_twos_complement_hex(&digest)
}

fn signed_twos_complement_hex(digest: &[u8]) -> String {
    let negative = digest.first().is_some_and(|byte| byte & 0x80 != 0);
    let mut magnitude = digest.to_vec();
    if negative {
        for byte in &mut magnitude {
            *byte = !*byte;
        }
        for byte in magnitude.iter_mut().rev() {
            let (value, carry) = byte.overflowing_add(1);
            *byte = value;
            if !carry {
                break;
            }
        }
    }
    let first = magnitude
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(magnitude.len());
    if first == magnitude.len() {
        return "0".to_owned();
    }
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    let mut bytes = magnitude[first..].iter();
    if let Some(byte) = bytes.next() {
        output.push_str(&format!("{byte:x}"));
    }
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::minecraft_server_hash;

    #[test]
    fn java_big_integer_hash_vectors_cover_positive_and_negative() {
        assert_eq!(
            minecraft_server_hash("Notch", &[], &[]),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
        assert_eq!(
            minecraft_server_hash("jeb_", &[], &[]),
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        );
        assert_eq!(
            minecraft_server_hash("simon", &[], &[]),
            "88e16a1019277b15d58faf0541e11910eb756f6"
        );
    }
}
