use crate::CodecError;

/// Modern Java protocol block Position packed into X:26, Z:26, Y:12 bits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockPosition {
    x: i32,
    y: i32,
    z: i32,
}

impl BlockPosition {
    pub const MIN_XZ: i32 = -(1 << 25);
    pub const MAX_XZ: i32 = (1 << 25) - 1;
    pub const MIN_Y: i32 = -(1 << 11);
    pub const MAX_Y: i32 = (1 << 11) - 1;

    pub fn new(x: i32, y: i32, z: i32) -> Result<Self, CodecError> {
        validate_coordinate("x", x, Self::MIN_XZ, Self::MAX_XZ)?;
        validate_coordinate("y", y, Self::MIN_Y, Self::MAX_Y)?;
        validate_coordinate("z", z, Self::MIN_XZ, Self::MAX_XZ)?;
        Ok(Self { x, y, z })
    }

    #[must_use]
    pub const fn x(self) -> i32 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> i32 {
        self.y
    }

    #[must_use]
    pub const fn z(self) -> i32 {
        self.z
    }

    #[must_use]
    pub const fn to_packed(self) -> u64 {
        let x = (self.x as u64) & 0x03ff_ffff;
        let z = (self.z as u64) & 0x03ff_ffff;
        let y = (self.y as u64) & 0x0fff;
        (x << 38) | (z << 12) | y
    }

    #[must_use]
    pub const fn from_packed(value: u64) -> Self {
        let x = sign_extend((value >> 38) as u32, 26);
        let z = sign_extend(((value >> 12) & 0x03ff_ffff) as u32, 26);
        let y = sign_extend((value & 0x0fff) as u32, 12);
        Self { x, y, z }
    }
}

const fn sign_extend(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

fn validate_coordinate(
    axis: &'static str,
    value: i32,
    min: i32,
    max: i32,
) -> Result<(), CodecError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(CodecError::InvalidBlockPosition {
            axis,
            value,
            min,
            max,
        })
    }
}
