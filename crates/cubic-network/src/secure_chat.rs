use std::{
    collections::VecDeque,
    time::{SystemTime, UNIX_EPOCH},
};

use cubic_auth::{AuthError, PlayerCertificate};
use cubic_protocol::{
    ProtocolUuid,
    bootstrap::v775::{ChatLastSeenUpdate, MessageSignature},
};
use thiserror::Error;

pub(crate) trait ChatCertificate: Send + Sync {
    fn public_key_der(&self) -> &[u8];
    fn public_key_signature(&self) -> &[u8];
    fn expires_at(&self) -> SystemTime;
    fn is_expired(&self, now: SystemTime) -> bool;
    fn sign_chat(&self, input: &[u8]) -> Result<[u8; 256], AuthError>;
}

impl ChatCertificate for PlayerCertificate {
    fn public_key_der(&self) -> &[u8] {
        self.public_key_der()
    }

    fn public_key_signature(&self) -> &[u8] {
        self.public_key_signature_v2()
    }

    fn expires_at(&self) -> SystemTime {
        self.expires_at()
    }

    fn is_expired(&self, now: SystemTime) -> bool {
        self.is_expired(now)
    }

    fn sign_chat(&self, input: &[u8]) -> Result<[u8; 256], AuthError> {
        self.sign_chat(input)
    }
}

/// Version-selected rules consumed by the generic secure-chat state machine.
/// Packet IDs and wire layouts remain in the corresponding protocol profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SecureChatRules {
    chain_version: i32,
    last_seen_capacity: usize,
    standalone_acknowledgement_threshold: i32,
}

impl SecureChatRules {
    pub(crate) const fn new(
        chain_version: i32,
        last_seen_capacity: usize,
        standalone_acknowledgement_threshold: i32,
    ) -> Self {
        Self {
            chain_version,
            last_seen_capacity,
            standalone_acknowledgement_threshold,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum SecureChatError {
    #[error(transparent)]
    Authentication(#[from] AuthError),
    #[error("the player chat certificate has expired")]
    ExpiredCertificate,
    #[error("invalid player-chat sequence: expected global index {expected}, received {received}")]
    IncomingSequence { expected: i32, received: i32 },
    #[error("secure-chat state exceeded its numeric range")]
    NumericRange,
    #[error("secure-chat acknowledgement window is incompatible with protocol 775")]
    IncompatibleAcknowledgementWindow,
}

pub(crate) struct SecureChatSession {
    rules: SecureChatRules,
    certificate: Box<dyn ChatCertificate>,
    sender: ProtocolUuid,
    sequence: SecureChatSequence,
}

struct SecureChatSequence {
    session: ProtocolUuid,
    next_outgoing_index: i32,
    next_incoming_index: i32,
    last_seen: LastSeenTracker,
}

impl SecureChatSequence {
    fn new(session: ProtocolUuid, capacity: usize) -> Self {
        Self {
            session,
            next_outgoing_index: 0,
            next_incoming_index: 0,
            last_seen: LastSeenTracker::new(capacity),
        }
    }

    fn reset(&mut self, session: ProtocolUuid, capacity: usize) {
        *self = Self::new(session, capacity);
    }
}

pub(crate) struct PreparedSignedMessage {
    pub(crate) signature: MessageSignature,
    pub(crate) last_seen_update: ChatLastSeenUpdate,
}

impl SecureChatSession {
    pub(crate) fn new(
        rules: SecureChatRules,
        certificate: PlayerCertificate,
        sender: ProtocolUuid,
        session: ProtocolUuid,
    ) -> Self {
        Self {
            rules,
            certificate: Box::new(certificate),
            sender,
            sequence: SecureChatSequence::new(session, rules.last_seen_capacity),
        }
    }

    pub(crate) const fn session_id(&self) -> ProtocolUuid {
        self.sequence.session
    }

    #[cfg(test)]
    pub(crate) fn with_certificate(
        rules: SecureChatRules,
        certificate: Box<dyn ChatCertificate>,
        sender: ProtocolUuid,
        session: ProtocolUuid,
    ) -> Self {
        Self {
            rules,
            certificate,
            sender,
            sequence: SecureChatSequence::new(session, rules.last_seen_capacity),
        }
    }

    pub(crate) fn certificate(&self) -> &dyn ChatCertificate {
        self.certificate.as_ref()
    }

    pub(crate) fn reset(&mut self, session: ProtocolUuid) {
        self.sequence.reset(session, self.rules.last_seen_capacity);
    }

    pub(crate) fn accept_incoming<F>(
        &mut self,
        global_index: i32,
        signature: Option<MessageSignature>,
        display: F,
    ) -> Result<(), SecureChatError>
    where
        F: FnOnce() -> bool,
    {
        advance_incoming_index(&mut self.sequence.next_incoming_index, global_index)?;
        let displayed = display();
        if let Some(signature) = signature {
            self.sequence.last_seen.add(signature, displayed)?;
        }
        Ok(())
    }

    pub(crate) fn standalone_acknowledgement(&mut self) -> Option<i32> {
        if self.sequence.last_seen.offset > self.rules.standalone_acknowledgement_threshold {
            Some(self.sequence.last_seen.take_offset())
        } else {
            None
        }
    }

    pub(crate) fn prepare_outgoing(
        &mut self,
        message: &str,
        timestamp_millis: i64,
        salt: i64,
    ) -> Result<PreparedSignedMessage, SecureChatError> {
        if self.certificate.is_expired(SystemTime::now()) {
            return Err(SecureChatError::ExpiredCertificate);
        }
        let (last_seen_update, last_seen_signatures) = self.sequence.last_seen.generate_update()?;
        let input = build_signing_input(SigningContext {
            rules: self.rules,
            sender: self.sender,
            session: self.sequence.session,
            message_index: self.sequence.next_outgoing_index,
            message,
            timestamp_millis,
            salt,
            last_seen: &last_seen_signatures,
        })?;
        let signature = MessageSignature::new(self.certificate.sign_chat(&input)?);
        self.sequence.next_outgoing_index = self
            .sequence
            .next_outgoing_index
            .checked_add(1)
            .ok_or(SecureChatError::NumericRange)?;
        Ok(PreparedSignedMessage {
            signature,
            last_seen_update,
        })
    }
}

fn advance_incoming_index(expected: &mut i32, received: i32) -> Result<(), SecureChatError> {
    if received != *expected {
        return Err(SecureChatError::IncomingSequence {
            expected: *expected,
            received,
        });
    }
    *expected = expected
        .checked_add(1)
        .ok_or(SecureChatError::NumericRange)?;
    Ok(())
}

struct LastSeenTracker {
    entries: VecDeque<Option<MessageSignature>>,
    last_added: Option<MessageSignature>,
    offset: i32,
}

impl LastSeenTracker {
    fn new(capacity: usize) -> Self {
        Self {
            entries: std::iter::repeat_n(None, capacity).collect(),
            last_added: None,
            offset: 0,
        }
    }

    fn add(&mut self, signature: MessageSignature, displayed: bool) -> Result<(), SecureChatError> {
        if self.last_added.as_ref() == Some(&signature) {
            return Ok(());
        }
        self.last_added = Some(signature.clone());
        let _ = self.entries.pop_front();
        self.entries.push_back(displayed.then_some(signature));
        self.offset = self
            .offset
            .checked_add(1)
            .ok_or(SecureChatError::NumericRange)?;
        Ok(())
    }

    fn take_offset(&mut self) -> i32 {
        std::mem::take(&mut self.offset)
    }

    fn generate_update(
        &mut self,
    ) -> Result<(ChatLastSeenUpdate, Vec<MessageSignature>), SecureChatError> {
        if self.entries.len() != 20 {
            return Err(SecureChatError::IncompatibleAcknowledgementWindow);
        }
        let mut acknowledged = [0_u8; 3];
        let mut signatures = Vec::with_capacity(self.entries.len());
        for (index, signature) in self.entries.iter().enumerate() {
            if let Some(signature) = signature {
                acknowledged[index / 8] |= 1 << (index % 8);
                signatures.push(signature.clone());
            }
        }
        let checksum = acknowledgement_checksum(&signatures);
        let update = ChatLastSeenUpdate::new(self.take_offset(), acknowledged, checksum)
            .map_err(|_| SecureChatError::NumericRange)?;
        Ok((update, signatures))
    }
}

fn acknowledgement_checksum(signatures: &[MessageSignature]) -> u8 {
    let mut list_hash = 1_i32;
    for signature in signatures {
        let mut array_hash = 1_i32;
        for byte in signature.as_bytes() {
            array_hash = array_hash
                .wrapping_mul(31)
                .wrapping_add(i32::from(i8::from_ne_bytes([*byte])));
        }
        list_hash = list_hash.wrapping_mul(31).wrapping_add(array_hash);
    }
    let checksum = list_hash as u8;
    if checksum == 0 { 1 } else { checksum }
}

struct SigningContext<'a> {
    rules: SecureChatRules,
    sender: ProtocolUuid,
    session: ProtocolUuid,
    message_index: i32,
    message: &'a str,
    timestamp_millis: i64,
    salt: i64,
    last_seen: &'a [MessageSignature],
}

fn build_signing_input(context: SigningContext<'_>) -> Result<Vec<u8>, SecureChatError> {
    let SigningContext {
        rules,
        sender,
        session,
        message_index,
        message,
        timestamp_millis,
        salt,
        last_seen,
    } = context;
    let message_length = i32::try_from(message.len()).map_err(|_| SecureChatError::NumericRange)?;
    let last_seen_length =
        i32::try_from(last_seen.len()).map_err(|_| SecureChatError::NumericRange)?;
    let capacity =
        4 + 16 + 16 + 4 + 8 + 8 + 4 + message.len() + 4 + last_seen.len().saturating_mul(256);
    let mut input = Vec::with_capacity(capacity);
    input.extend_from_slice(&rules.chain_version.to_be_bytes());
    input.extend_from_slice(&sender.to_bytes());
    input.extend_from_slice(&session.to_bytes());
    input.extend_from_slice(&message_index.to_be_bytes());
    input.extend_from_slice(&salt.to_be_bytes());
    input.extend_from_slice(&(timestamp_millis / 1_000).to_be_bytes());
    input.extend_from_slice(&message_length.to_be_bytes());
    input.extend_from_slice(message.as_bytes());
    input.extend_from_slice(&last_seen_length.to_be_bytes());
    for signature in last_seen {
        input.extend_from_slice(signature.as_bytes());
    }
    Ok(input)
}

pub(crate) fn system_time_millis(value: SystemTime) -> Result<i64, SecureChatError> {
    let duration = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SecureChatError::NumericRange)?;
    i64::try_from(duration.as_millis()).map_err(|_| SecureChatError::NumericRange)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_775_signing_input_is_byte_exact() {
        let signature = MessageSignature::new([0x5a; 256]);
        let input = build_signing_input(SigningContext {
            rules: SecureChatRules::new(1, 20, 64),
            sender: ProtocolUuid::from_u128(1),
            session: ProtocolUuid::from_u128(2),
            message_index: 3,
            message: "Hi",
            timestamp_millis: 4_999,
            salt: 5,
            last_seen: &[signature],
        })
        .unwrap();
        assert_eq!(&input[0..4], &1_i32.to_be_bytes());
        assert_eq!(&input[4..20], &1_u128.to_be_bytes());
        assert_eq!(&input[20..36], &2_u128.to_be_bytes());
        assert_eq!(&input[36..40], &3_i32.to_be_bytes());
        assert_eq!(&input[40..48], &5_i64.to_be_bytes());
        assert_eq!(&input[48..56], &4_i64.to_be_bytes());
        assert_eq!(&input[56..60], &2_i32.to_be_bytes());
        assert_eq!(&input[60..62], b"Hi");
        assert_eq!(&input[62..66], &1_i32.to_be_bytes());
        assert_eq!(&input[66..], &[0x5a; 256]);
    }

    #[test]
    fn alternate_profile_changes_core_signing_without_raw_packet_ids() {
        let standard = build_signing_input(SigningContext {
            rules: SecureChatRules::new(1, 20, 64),
            sender: ProtocolUuid::from_u128(0),
            session: ProtocolUuid::from_u128(0),
            message_index: 0,
            message: "x",
            timestamp_millis: 0,
            salt: 0,
            last_seen: &[],
        })
        .unwrap();
        let alternate = build_signing_input(SigningContext {
            rules: SecureChatRules::new(7, 8, 2),
            sender: ProtocolUuid::from_u128(0),
            session: ProtocolUuid::from_u128(0),
            message_index: 0,
            message: "x",
            timestamp_millis: 0,
            salt: 0,
            last_seen: &[],
        })
        .unwrap();
        assert_ne!(&standard[..4], &alternate[..4]);
    }

    #[test]
    fn last_seen_window_is_bounded_deduplicated_and_checksums_are_nonzero() {
        let mut tracker = LastSeenTracker::new(20);
        let signature = MessageSignature::new([1; 256]);
        tracker.add(signature.clone(), true).unwrap();
        tracker.add(signature.clone(), true).unwrap();
        assert_eq!(tracker.offset, 1);
        let (update, messages) = tracker.generate_update().unwrap();
        assert_eq!(messages, [signature]);
        assert_eq!(update.offset(), 1);
        assert_eq!(update.acknowledged(), [0, 0, 8]);
        assert_eq!(update.checksum(), 32);
        assert_eq!(tracker.offset, 0);
    }

    #[test]
    fn incoming_indices_are_strict_and_resettable_state_is_bounded() {
        assert_eq!(SecureChatRules::new(1, 20, 64).last_seen_capacity, 20);
        let mut tracker = LastSeenTracker::new(20);
        for byte in 0..=u8::MAX {
            tracker
                .add(MessageSignature::new([byte; 256]), byte % 2 == 0)
                .unwrap();
        }
        assert_eq!(tracker.entries.len(), 20);

        let mut expected = 0;
        advance_incoming_index(&mut expected, 0).unwrap();
        assert!(matches!(
            advance_incoming_index(&mut expected, 0),
            Err(SecureChatError::IncomingSequence {
                expected: 1,
                received: 0
            })
        ));
        assert!(matches!(
            advance_incoming_index(&mut expected, 2),
            Err(SecureChatError::IncomingSequence {
                expected: 1,
                received: 2
            })
        ));
        assert_eq!(expected, 1);

        let first = ProtocolUuid::from_u128(1);
        let second = ProtocolUuid::from_u128(2);
        let mut sequence = SecureChatSequence::new(first, 20);
        sequence.next_outgoing_index = 8;
        sequence.next_incoming_index = 9;
        sequence.last_seen.offset = 10;
        sequence.reset(second, 20);
        assert_eq!(sequence.session, second);
        assert_eq!(sequence.next_outgoing_index, 0);
        assert_eq!(sequence.next_incoming_index, 0);
        assert_eq!(sequence.last_seen.offset, 0);
        assert_eq!(sequence.last_seen.entries.len(), 20);
    }

    #[test]
    fn messages_dropped_before_display_are_not_acknowledged_as_seen() {
        let mut tracker = LastSeenTracker::new(20);
        tracker.add(MessageSignature::new([9; 256]), false).unwrap();
        let (update, signatures) = tracker.generate_update().unwrap();
        assert!(signatures.is_empty());
        assert_eq!(update.acknowledged(), [0; 3]);
        assert_eq!(update.checksum(), 1);
    }
}
