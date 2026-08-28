use cubic_protocol::bootstrap::v775;
use cubic_version::{MinecraftVersionId, ProtocolVersion, VersionError};

use crate::secure_chat::SecureChatRules;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DevLoginProtocolProfile {
    minecraft_version: MinecraftVersionId,
    protocol_version: ProtocolVersion,
}

impl DevLoginProtocolProfile {
    pub(crate) fn protocol_775() -> Result<Self, VersionError> {
        Ok(Self {
            minecraft_version: MinecraftVersionId::new(v775::MINECRAFT_VERSION_ID)?,
            protocol_version: ProtocolVersion::new(v775::PROTOCOL_VERSION),
        })
    }

    pub(crate) fn minecraft_version(&self) -> &MinecraftVersionId {
        &self.minecraft_version
    }

    pub(crate) const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Current secure-chat behavior belongs to this replaceable version profile.
    pub(crate) const fn secure_chat_rules(&self) -> SecureChatRules {
        SecureChatRules::new(1, v775::MAX_LAST_SEEN_MESSAGES, 64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_identity_selects_its_secure_chat_rules() {
        let profile = DevLoginProtocolProfile::protocol_775().unwrap();
        assert_eq!(profile.secure_chat_rules(), SecureChatRules::new(1, 20, 64));
    }
}
