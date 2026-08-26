/// Minecraft UUID represented by its exact 128 wire bits.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProtocolUuid([u8; 16]);

impl ProtocolUuid {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }

    #[must_use]
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0
    }

    #[must_use]
    pub const fn as_u128(self) -> u128 {
        u128::from_be_bytes(self.0)
    }
}
