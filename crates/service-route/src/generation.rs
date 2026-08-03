use crate::ProviderGeneration;

pub const SERVICE_GENERATION_MAGIC: [u8; 4] = *b"NSGN";
pub const SERVICE_GENERATION_VERSION: u16 = 1;
pub const SERVICE_GENERATION_WIRE_BYTES: usize = 16;

/// Indicates that every nonzero provider generation has been issued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderGenerationExhausted;

/// Manager-owned source of process-independent provider generations.
///
/// A new sequence first issues generation 1. Each successful call advances the sequence exactly
/// once, and the sequence remains exhausted after issuing [`u64::MAX`].
#[derive(Debug, PartialEq, Eq)]
pub struct ProviderGenerationSequence {
    last_issued: Option<ProviderGeneration>,
}

impl ProviderGenerationSequence {
    /// Creates a sequence whose first issued generation is 1.
    pub const fn new() -> Self {
        Self { last_issued: None }
    }

    /// Resumes a sequence strictly after an already-issued generation.
    pub const fn after(last_issued: ProviderGeneration) -> Self {
        Self {
            last_issued: Some(last_issued),
        }
    }

    /// Returns the most recently issued generation, if any.
    pub const fn last_issued(&self) -> Option<ProviderGeneration> {
        self.last_issued
    }

    /// Issues the next generation without wrapping.
    pub fn next_generation(&mut self) -> Result<ProviderGeneration, ProviderGenerationExhausted> {
        let value = match self.last_issued {
            None => 1,
            Some(last_issued) => last_issued
                .get()
                .checked_add(1)
                .ok_or(ProviderGenerationExhausted)?,
        };
        let generation = ProviderGeneration::new(value).expect("sequence always produces nonzero");
        self.last_issued = Some(generation);
        Ok(generation)
    }
}

impl Default for ProviderGenerationSequence {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact service-generation state transferred between generation authorities.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceGenerationHandoff {
    generation: ProviderGeneration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceGenerationDecodeError {
    InvalidLength,
    InvalidMagic,
    UnsupportedVersion,
    NonzeroReserved,
    ZeroGeneration,
}

impl ServiceGenerationHandoff {
    pub const fn new(generation: ProviderGeneration) -> Self {
        Self { generation }
    }

    pub const fn generation(self) -> ProviderGeneration {
        self.generation
    }

    /// Encodes this handoff into the exact 16-byte `NSGN` v1 representation.
    pub fn encode(self) -> [u8; SERVICE_GENERATION_WIRE_BYTES] {
        let mut output = [0; SERVICE_GENERATION_WIRE_BYTES];
        output[0..4].copy_from_slice(&SERVICE_GENERATION_MAGIC);
        output[4..6].copy_from_slice(&SERVICE_GENERATION_VERSION.to_le_bytes());
        output[8..16].copy_from_slice(&self.generation.get().to_le_bytes());
        output
    }

    /// Decodes one exact canonical 16-byte `NSGN` v1 handoff.
    pub fn decode(input: &[u8]) -> Result<Self, ServiceGenerationDecodeError> {
        if input.len() != SERVICE_GENERATION_WIRE_BYTES {
            return Err(ServiceGenerationDecodeError::InvalidLength);
        }
        if input[0..4] != SERVICE_GENERATION_MAGIC {
            return Err(ServiceGenerationDecodeError::InvalidMagic);
        }
        if u16::from_le_bytes([input[4], input[5]]) != SERVICE_GENERATION_VERSION {
            return Err(ServiceGenerationDecodeError::UnsupportedVersion);
        }
        if input[6] != 0 || input[7] != 0 {
            return Err(ServiceGenerationDecodeError::NonzeroReserved);
        }
        let generation = u64::from_le_bytes([
            input[8], input[9], input[10], input[11], input[12], input[13], input[14], input[15],
        ]);
        let generation = ProviderGeneration::new(generation)
            .ok_or(ServiceGenerationDecodeError::ZeroGeneration)?;
        Ok(Self::new(generation))
    }
}
