//! Explicit little-endian integer encoding helpers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeU16([u8; 2]);

impl LeU16 {
    pub const fn new(value: u16) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u16 {
        u16::from_le_bytes(self.0)
    }

    pub const fn bytes(self) -> [u8; 2] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeU32([u8; 4]);

impl LeU32 {
    pub const fn new(value: u32) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u32 {
        u32::from_le_bytes(self.0)
    }

    pub const fn bytes(self) -> [u8; 4] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeU64([u8; 8]);

impl LeU64 {
    pub const fn new(value: u64) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u64 {
        u64::from_le_bytes(self.0)
    }

    pub const fn bytes(self) -> [u8; 8] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{LeU16, LeU32, LeU64};

    #[test]
    fn values_have_stable_little_endian_bytes() {
        assert_eq!(LeU16::new(0x1234).bytes(), [0x34, 0x12]);
        assert_eq!(LeU32::new(0x1234_5678).bytes(), [0x78, 0x56, 0x34, 0x12]);
        assert_eq!(
            LeU64::new(0x0123_4567_89ab_cdef).bytes(),
            [0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(LeU16::new(0x1234).get(), 0x1234);
        assert_eq!(LeU32::new(0x1234_5678).get(), 0x1234_5678);
        assert_eq!(
            LeU64::new(0x0123_4567_89ab_cdef).get(),
            0x0123_4567_89ab_cdef
        );
    }
}
