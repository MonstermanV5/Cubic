use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{Client, redirect::Policy};
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1v15::{Signature, SigningKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePublicKey},
    signature::{SignatureEncoding, Signer},
    traits::PublicKeyParts,
};
use serde_json::{Map, Value};
use sha2::Sha256;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroizing;

use crate::{AuthClientOptions, AuthError, AuthStage, AuthenticatedMinecraftAccount, SecretString};

const PLAYER_CERTIFICATES: &str = "https://api.minecraftservices.com/player/certificates";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_PEM_BYTES: usize = 16 * 1024;
const MAX_KEY_SIGNATURE_BYTES: usize = 4096;
const MAX_KEY_SIGNATURE_BASE64_BYTES: usize = 5_464;
const RSA_BITS: usize = 2048;

/// A short-lived Mojang-issued RSA key pair used only for player chat.
pub struct PlayerCertificate {
    signing_key: SigningKey<Sha256>,
    public_key_der: Vec<u8>,
    public_key_signature_v2: Vec<u8>,
    expires_at: SystemTime,
    refreshed_after: SystemTime,
}

impl fmt::Debug for PlayerCertificate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayerCertificate")
            .field("signing_key", &"[REDACTED]")
            .field("public_key_der_bytes", &self.public_key_der.len())
            .field("key_signature_bytes", &self.public_key_signature_v2.len())
            .field("expires_at", &self.expires_at)
            .field("refreshed_after", &self.refreshed_after)
            .finish()
    }
}

impl PlayerCertificate {
    #[must_use]
    pub fn public_key_der(&self) -> &[u8] {
        &self.public_key_der
    }

    #[must_use]
    pub fn public_key_signature_v2(&self) -> &[u8] {
        &self.public_key_signature_v2
    }

    #[must_use]
    pub const fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    #[must_use]
    pub const fn refreshed_after(&self) -> SystemTime {
        self.refreshed_after
    }

    #[must_use]
    pub fn should_refresh(&self, now: SystemTime) -> bool {
        now >= self.refreshed_after
    }

    #[must_use]
    pub fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at
    }

    pub fn sign_chat(&self, input: &[u8]) -> Result<[u8; 256], AuthError> {
        let signature: Signature = self.signing_key.sign(input);
        signature
            .to_bytes()
            .as_ref()
            .try_into()
            .map_err(|_| AuthError::PlayerChatSigning)
    }

    fn parse(body: &[u8], now: SystemTime) -> Result<Self, AuthError> {
        let mut raw: Value =
            serde_json::from_slice(body).map_err(|_| AuthError::MalformedPlayerCertificateJson)?;
        let root = raw
            .as_object_mut()
            .ok_or(AuthError::InvalidPlayerCertificateFieldType {
                field: "<root>",
                expected: "object",
            })?;
        ensure_known_fields(
            root,
            &[
                "keyPair",
                "publicKeySignature",
                "publicKeySignatureV2",
                "expiresAt",
                "refreshedAfter",
            ],
        )?;
        let (private_key_pem, public_key_pem) = {
            let key_pair = required_object_mut(root, "keyPair")?;
            ensure_known_fields(key_pair, &["privateKey", "publicKey"])?;
            let private_key_pem = take_secret_string(key_pair, "privateKey")?;
            let public_key_pem = required_string(key_pair, "publicKey")?.to_owned();
            (private_key_pem, public_key_pem)
        };
        let public_key_signature_v2 = required_string(root, "publicKeySignatureV2")?.to_owned();
        let expires_at_text = required_string(root, "expiresAt")?.to_owned();
        let refreshed_after_text = required_string(root, "refreshedAfter")?.to_owned();

        // Minecraft Services currently also returns the pre-1.19.1 signature.
        // Protocol 775 uses V2, but accepting this known optional sibling keeps
        // the strict parser compatible with the live service response.
        if root.contains_key("publicKeySignature") {
            let legacy_signature = required_string(root, "publicKeySignature")?;
            if legacy_signature.len() > MAX_KEY_SIGNATURE_BASE64_BYTES {
                return Err(AuthError::InvalidPlayerCertificateSignature);
            }
        }
        let private_der = decode_exact_pem(
            &private_key_pem,
            "-----BEGIN RSA PRIVATE KEY-----",
            "-----END RSA PRIVATE KEY-----",
            "keyPair.privateKey",
        )?;
        let public_der = decode_public_pem(
            &public_key_pem,
            "-----BEGIN RSA PUBLIC KEY-----",
            "-----END RSA PUBLIC KEY-----",
            "keyPair.publicKey",
        )?;
        let private_key = RsaPrivateKey::from_pkcs8_der(&private_der).map_err(|_| {
            AuthError::InvalidPlayerCertificateKey {
                reason: "private key is not RSA PKCS#8 DER",
            }
        })?;
        let public_key = RsaPublicKey::from_public_key_der(&public_der).map_err(|_| {
            AuthError::InvalidPlayerCertificateKey {
                reason: "public key is not RSA X.509 SubjectPublicKeyInfo DER",
            }
        })?;
        if private_key.n().bits() != RSA_BITS || public_key.n().bits() != RSA_BITS {
            return Err(AuthError::InvalidPlayerCertificateKey {
                reason: "keys are not RSA-2048",
            });
        }
        if RsaPublicKey::from(&private_key) != public_key {
            return Err(AuthError::InvalidPlayerCertificateKey {
                reason: "private and public keys do not match",
            });
        }
        let canonical_public_der = public_key
            .to_public_key_der()
            .map_err(|_| AuthError::InvalidPlayerCertificateKey {
                reason: "public key could not be canonically encoded",
            })?
            .as_bytes()
            .to_vec();
        if canonical_public_der != *public_der {
            return Err(AuthError::InvalidPlayerCertificateKey {
                reason: "public key DER is not canonical",
            });
        }
        if public_key_signature_v2.len() > MAX_KEY_SIGNATURE_BASE64_BYTES {
            return Err(AuthError::InvalidPlayerCertificateSignature);
        }
        let signature = STANDARD
            .decode(public_key_signature_v2.as_bytes())
            .map_err(|_| AuthError::InvalidPlayerCertificateSignature)?;
        if signature.is_empty() || signature.len() > MAX_KEY_SIGNATURE_BYTES {
            return Err(AuthError::InvalidPlayerCertificateSignature);
        }
        let expires_at = parse_timestamp(&expires_at_text, "expiresAt")?;
        let refreshed_after = parse_timestamp(&refreshed_after_text, "refreshedAfter")?;
        if refreshed_after > expires_at {
            return Err(AuthError::InvalidPlayerCertificateTimestamp {
                field: "refreshedAfter",
            });
        }
        if expires_at <= now {
            return Err(AuthError::ExpiredPlayerCertificate);
        }
        Ok(Self {
            signing_key: SigningKey::new(private_key),
            public_key_der: canonical_public_der,
            public_key_signature_v2: signature,
            expires_at,
            refreshed_after,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PlayerCertificateClient {
    client: Client,
}

impl PlayerCertificateClient {
    pub fn new(options: AuthClientOptions) -> Result<Self, AuthError> {
        let client = Client::builder()
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .redirect(Policy::none())
            .user_agent("Cubic/0.1 (native Minecraft client)")
            .build()
            .map_err(|source| AuthError::Transport {
                stage: AuthStage::PlayerCertificates,
                source,
            })?;
        Ok(Self { client })
    }

    pub async fn request(
        &self,
        account: &AuthenticatedMinecraftAccount,
    ) -> Result<PlayerCertificate, AuthError> {
        let bearer = format!("Bearer {}", account.minecraft_access_token.expose());
        let mut response = self
            .client
            .post(PLAYER_CERTIFICATES)
            .header("Authorization", bearer)
            .send()
            .await
            .map_err(|source| AuthError::Transport {
                stage: AuthStage::PlayerCertificates,
                source,
            })?;
        let status = response.status();
        let mut body = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| AuthError::Transport {
                stage: AuthStage::PlayerCertificates,
                source,
            })?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(AuthError::ResponseTooLarge {
                    stage: AuthStage::PlayerCertificates,
                    limit: MAX_RESPONSE_BYTES,
                });
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(AuthError::Http {
                stage: AuthStage::PlayerCertificates,
                status: status.as_u16(),
                message: safe_service_error(&body),
            });
        }
        PlayerCertificate::parse(&body, SystemTime::now())
    }
}

fn required_object_mut<'a>(
    object: &'a mut Map<String, Value>,
    field: &'static str,
) -> Result<&'a mut Map<String, Value>, AuthError> {
    object
        .get_mut(field)
        .ok_or(AuthError::MissingPlayerCertificateField { field })?
        .as_object_mut()
        .ok_or(AuthError::InvalidPlayerCertificateFieldType {
            field,
            expected: "object",
        })
}

fn take_secret_string(
    object: &mut Map<String, Value>,
    field: &'static str,
) -> Result<SecretString, AuthError> {
    match object.remove(field) {
        Some(Value::String(value)) => Ok(SecretString::new(value)),
        Some(_) => Err(AuthError::InvalidPlayerCertificateFieldType {
            field,
            expected: "string",
        }),
        None => Err(AuthError::MissingPlayerCertificateField { field }),
    }
}

fn ensure_known_fields(object: &Map<String, Value>, known: &[&str]) -> Result<(), AuthError> {
    if object.keys().any(|key| !known.contains(&key.as_str())) {
        return Err(AuthError::UnexpectedPlayerCertificateField);
    }
    Ok(())
}

fn safe_service_error(body: &[u8]) -> String {
    let Ok(Value::Object(object)) = serde_json::from_slice(body) else {
        return "player certificate request was rejected".to_owned();
    };
    let mut details = Vec::new();
    for (label, key) in [
        ("error", "error"),
        ("type", "errorType"),
        ("message", "errorMessage"),
    ] {
        if let Some(value) = object.get(key).and_then(Value::as_str) {
            let safe: String = value
                .chars()
                .filter(|character| !character.is_control())
                .take(160)
                .collect();
            if !safe.is_empty() {
                details.push(format!("{label}={safe}"));
            }
        }
    }
    if details.is_empty() {
        "player certificate request was rejected".to_owned()
    } else {
        format!(
            "player certificate request was rejected ({})",
            details.join(", ")
        )
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, AuthError> {
    object
        .get(field)
        .ok_or(AuthError::MissingPlayerCertificateField { field })?
        .as_str()
        .ok_or(AuthError::InvalidPlayerCertificateFieldType {
            field,
            expected: "string",
        })
}

fn decode_exact_pem(
    value: &SecretString,
    begin: &str,
    end: &str,
    field: &'static str,
) -> Result<Zeroizing<Vec<u8>>, AuthError> {
    let value = value.expose().trim();
    if value.len() > MAX_PEM_BYTES || !value.starts_with(begin) || !value.ends_with(end) {
        return Err(AuthError::InvalidPlayerCertificatePem { field });
    }
    let encoded = value
        .strip_prefix(begin)
        .and_then(|value| value.strip_suffix(end))
        .ok_or(AuthError::InvalidPlayerCertificatePem { field })?;
    let compact = Zeroizing::new(
        encoded
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>(),
    );
    if compact.is_empty() {
        return Err(AuthError::InvalidPlayerCertificatePem { field });
    }
    STANDARD
        .decode(compact.as_bytes())
        .map(Zeroizing::new)
        .map_err(|_| AuthError::InvalidPlayerCertificatePem { field })
}

fn decode_public_pem(
    value: &str,
    begin: &str,
    end: &str,
    field: &'static str,
) -> Result<Vec<u8>, AuthError> {
    let value = value.trim();
    if value.len() > MAX_PEM_BYTES || !value.starts_with(begin) || !value.ends_with(end) {
        return Err(AuthError::InvalidPlayerCertificatePem { field });
    }
    let encoded = value
        .strip_prefix(begin)
        .and_then(|value| value.strip_suffix(end))
        .ok_or(AuthError::InvalidPlayerCertificatePem { field })?;
    let compact: String = encoded
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if compact.is_empty() {
        return Err(AuthError::InvalidPlayerCertificatePem { field });
    }
    STANDARD
        .decode(compact.as_bytes())
        .map_err(|_| AuthError::InvalidPlayerCertificatePem { field })
}

fn parse_timestamp(value: &str, field: &'static str) -> Result<SystemTime, AuthError> {
    if value.len() > 64 {
        return Err(AuthError::InvalidPlayerCertificateTimestamp { field });
    }
    let parsed = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| AuthError::InvalidPlayerCertificateTimestamp { field })?;
    let nanos = parsed.unix_timestamp_nanos();
    if nanos < 0 {
        return Err(AuthError::InvalidPlayerCertificateTimestamp { field });
    }
    let seconds = u64::try_from(nanos / 1_000_000_000)
        .map_err(|_| AuthError::InvalidPlayerCertificateTimestamp { field })?;
    let subsec = u32::try_from(nanos % 1_000_000_000)
        .map_err(|_| AuthError::InvalidPlayerCertificateTimestamp { field })?;
    UNIX_EPOCH
        .checked_add(Duration::new(seconds, subsec))
        .ok_or(AuthError::InvalidPlayerCertificateTimestamp { field })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::{
        pkcs1v15::VerifyingKey,
        pkcs8::{EncodePrivateKey, EncodePublicKey},
        rand_core::{CryptoRng, Error as RandError, RngCore},
        signature::Verifier,
    };
    use serde_json::json;

    fn fixture(expires: &str, refresh: &str) -> Vec<u8> {
        fixture_with_options(expires, refresh, RSA_BITS, true)
    }

    fn fixture_with_bits(expires: &str, refresh: &str, bits: usize) -> Vec<u8> {
        fixture_with_options(expires, refresh, bits, true)
    }

    fn fixture_with_options(
        expires: &str,
        refresh: &str,
        bits: usize,
        include_legacy_signature: bool,
    ) -> Vec<u8> {
        let private =
            RsaPrivateKey::new(&mut DeterministicRng::new(0x4355_4249_4354_4553), bits).unwrap();
        let public = RsaPublicKey::from(&private);
        let private_der = private.to_pkcs8_der().unwrap();
        let public_der = public.to_public_key_der().unwrap();
        let private_pem = pem("RSA PRIVATE KEY", private_der.as_bytes());
        let public_pem = pem("RSA PUBLIC KEY", public_der.as_bytes());
        let mut value = json!({
            "keyPair": { "privateKey": private_pem, "publicKey": public_pem },
            "publicKeySignatureV2": STANDARD.encode([7_u8; 256]),
            "expiresAt": expires,
            "refreshedAfter": refresh
        });
        if include_legacy_signature {
            value["publicKeySignature"] = json!(STANDARD.encode([3_u8; 256]));
        }
        serde_json::to_vec(&value).unwrap()
    }

    struct DeterministicRng(u64);

    impl DeterministicRng {
        const fn new(seed: u64) -> Self {
            Self(seed)
        }
    }

    impl RngCore for DeterministicRng {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }

        fn next_u64(&mut self) -> u64 {
            let mut value = self.0;
            value ^= value << 13;
            value ^= value >> 7;
            value ^= value << 17;
            self.0 = value;
            value
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            for chunk in destination.chunks_mut(8) {
                let bytes = self.next_u64().to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), RandError> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for DeterministicRng {}

    fn pem(label: &str, der: &[u8]) -> String {
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----",
            STANDARD.encode(der)
        )
    }

    #[test]
    fn valid_certificate_parses_signs_and_redacts() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let certificate = PlayerCertificate::parse(
            &fixture("2030-01-01T00:00:00Z", "2029-12-31T00:00:00Z"),
            now,
        )
        .unwrap();
        assert_eq!(certificate.public_key_signature_v2().len(), 256);
        let first = certificate.sign_chat(b"deterministic input").unwrap();
        let second = certificate.sign_chat(b"deterministic input").unwrap();
        assert_eq!(first, second);
        let signature = Signature::try_from(first.as_slice()).unwrap();
        VerifyingKey::<Sha256>::new(
            RsaPublicKey::from_public_key_der(certificate.public_key_der()).unwrap(),
        )
        .verify(b"deterministic input", &signature)
        .unwrap();
        assert!(format!("{certificate:?}").contains("[REDACTED]"));
        assert!(!format!("{certificate:?}").contains("PRIVATE KEY"));
    }

    #[test]
    fn current_live_schema_accepts_optional_legacy_signature() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let with_legacy = fixture_with_options(
            "2030-01-01T00:00:00Z",
            "2029-12-31T00:00:00Z",
            RSA_BITS,
            true,
        );
        let without_legacy = fixture_with_options(
            "2030-01-01T00:00:00Z",
            "2029-12-31T00:00:00Z",
            RSA_BITS,
            false,
        );
        assert!(PlayerCertificate::parse(&with_legacy, now).is_ok());
        assert!(PlayerCertificate::parse(&without_legacy, now).is_ok());
    }

    #[test]
    fn schema_errors_identify_json_field_and_signature_failures() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(matches!(
            PlayerCertificate::parse(b"not json", now),
            Err(AuthError::MalformedPlayerCertificateJson)
        ));
        assert!(matches!(
            PlayerCertificate::parse(b"{}", now),
            Err(AuthError::MissingPlayerCertificateField { field: "keyPair" })
        ));

        let mut wrong_type: Value =
            serde_json::from_slice(&fixture("2030-01-01T00:00:00Z", "2029-12-31T00:00:00Z"))
                .unwrap();
        wrong_type["keyPair"] = json!("not an object");
        assert!(matches!(
            PlayerCertificate::parse(&serde_json::to_vec(&wrong_type).unwrap(), now),
            Err(AuthError::InvalidPlayerCertificateFieldType {
                field: "keyPair",
                expected: "object"
            })
        ));

        let mut bad_signature: Value =
            serde_json::from_slice(&fixture("2030-01-01T00:00:00Z", "2029-12-31T00:00:00Z"))
                .unwrap();
        bad_signature["publicKeySignatureV2"] = json!("not base64!");
        assert!(matches!(
            PlayerCertificate::parse(&serde_json::to_vec(&bad_signature).unwrap(), now),
            Err(AuthError::InvalidPlayerCertificateSignature)
        ));
    }

    #[test]
    fn service_errors_retain_only_bounded_safe_metadata() {
        let body = br#"{
            "error":"FORBIDDEN",
            "errorType":"INVALID_TOKEN",
            "errorMessage":"certificate unavailable\nretry"
        }"#;
        assert_eq!(
            safe_service_error(body),
            "player certificate request was rejected (error=FORBIDDEN, type=INVALID_TOKEN, message=certificate unavailableretry)"
        );
        assert_eq!(
            safe_service_error(b"not json"),
            "player certificate request was rejected"
        );
    }

    #[test]
    fn malformed_json_pem_key_type_and_timestamps_are_rejected() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(PlayerCertificate::parse(b"{}", now).is_err());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fixture("2030-01-01T00:00:00Z", "2029-12-31T00:00:00Z"))
                .unwrap();
        value["keyPair"]["privateKey"] = json!("not pem");
        assert!(PlayerCertificate::parse(&serde_json::to_vec(&value).unwrap(), now).is_err());
        assert!(
            PlayerCertificate::parse(
                &fixture_with_bits("2030-01-01T00:00:00Z", "2029-12-31T00:00:00Z", 1024),
                now,
            )
            .is_err()
        );
        let malformed_time = fixture("not-a-time", "2029-12-31T00:00:00Z");
        assert!(PlayerCertificate::parse(&malformed_time, now).is_err());
    }

    #[test]
    fn expired_and_refresh_after_rules_are_explicit() {
        let now = UNIX_EPOCH + Duration::from_secs(1_893_456_000);
        assert!(matches!(
            PlayerCertificate::parse(
                &fixture("2020-01-01T00:00:00Z", "2019-12-31T00:00:00Z"),
                now
            ),
            Err(AuthError::ExpiredPlayerCertificate)
        ));
        let certificate = PlayerCertificate::parse(
            &fixture("2031-01-01T00:00:00Z", "2029-01-01T00:00:00Z"),
            now,
        )
        .unwrap();
        assert!(certificate.should_refresh(now));
        assert!(!certificate.is_expired(now));
    }
}
