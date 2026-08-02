use core::{fmt, str::FromStr};

use crate::ProtocolIdError;

pub const PROTOCOL_ID_BYTES: usize = 16;
pub const PROTOCOL_ID_TEXT_BYTES: usize = 36;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolId([u8; PROTOCOL_ID_BYTES]);

impl ProtocolId {
    pub const fn from_bytes(bytes: [u8; PROTOCOL_ID_BYTES]) -> Result<Self, ProtocolIdError> {
        match validate_bytes(&bytes) {
            Ok(()) => Ok(Self(bytes)),
            Err(error) => Err(error),
        }
    }

    pub fn parse(text: &str) -> Result<Self, ProtocolIdError> {
        let source = text.as_bytes();
        if source.len() != PROTOCOL_ID_TEXT_BYTES {
            return Err(ProtocolIdError::InvalidLength);
        }
        for (index, byte) in source.iter().copied().enumerate() {
            if matches!(index, 8 | 13 | 18 | 23) {
                if byte != b'-' {
                    return Err(ProtocolIdError::NonCanonical);
                }
            } else if !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte) {
                return Err(if byte.is_ascii_hexdigit() {
                    ProtocolIdError::NonCanonical
                } else {
                    ProtocolIdError::InvalidHex
                });
            }
        }

        let mut bytes = [0_u8; PROTOCOL_ID_BYTES];
        let mut output = 0;
        let mut high = None;
        for byte in source.iter().copied().filter(|byte| *byte != b'-') {
            let nibble = hex_value(byte).ok_or(ProtocolIdError::InvalidHex)?;
            if let Some(first) = high.take() {
                bytes[output] = (first << 4) | nibble;
                output += 1;
            } else {
                high = Some(nibble);
            }
        }
        Self::from_bytes(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; PROTOCOL_ID_BYTES] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; PROTOCOL_ID_BYTES] {
        self.0
    }
}

impl FromStr for ProtocolId {
    type Err = ProtocolIdError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Self::parse(source)
    }
}

impl AsRef<[u8; PROTOCOL_ID_BYTES]> for ProtocolId {
    fn as_ref(&self) -> &[u8; PROTOCOL_ID_BYTES] {
        self.as_bytes()
    }
}

impl fmt::Display for ProtocolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().copied().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ProtocolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ProtocolId({self})")
    }
}

const fn validate_bytes(bytes: &[u8; PROTOCOL_ID_BYTES]) -> Result<(), ProtocolIdError> {
    let mut all_zero = true;
    let mut all_ones = true;
    let mut index = 0;
    while index < PROTOCOL_ID_BYTES {
        if bytes[index] != 0 {
            all_zero = false;
        }
        if bytes[index] != u8::MAX {
            all_ones = false;
        }
        index += 1;
    }
    if all_zero {
        return Err(ProtocolIdError::Nil);
    }
    if all_ones {
        return Err(ProtocolIdError::AllOnes);
    }
    if bytes[6] >> 4 != 4 {
        return Err(ProtocolIdError::InvalidVersion);
    }
    if bytes[8] & 0xc0 != 0x80 {
        return Err(ProtocolIdError::InvalidVariant);
    }
    Ok(())
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
