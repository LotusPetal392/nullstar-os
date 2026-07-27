//! CRC32C (Castagnoli) used by NullFS metadata.

const REVERSED_CASTAGNOLI_POLYNOMIAL: u32 = 0x82f6_3b78;

pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (REVERSED_CASTAGNOLI_POLYNOMIAL & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    #[test]
    fn matches_standard_check_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }
}
