use cubic_protocol::bootstrap::v775;
use cubic_version::{MinecraftVersionId, ProtocolVersion, VersionError};

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
}
