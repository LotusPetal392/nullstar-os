use core::{
    fmt,
    num::{NonZeroU32, NonZeroU64},
};

pub const SERVICE_ID_BYTES: usize = 16;

/// Stable UUIDv4 service identifier represented in RFC/network byte order.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceId([u8; SERVICE_ID_BYTES]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceIdError {
    Nil,
    InvalidVersion,
    InvalidVariant,
}

impl ServiceId {
    pub const fn from_bytes(bytes: [u8; SERVICE_ID_BYTES]) -> Result<Self, ServiceIdError> {
        let mut all_zero = true;
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                all_zero = false;
            }
            index += 1;
        }
        if all_zero {
            return Err(ServiceIdError::Nil);
        }
        if bytes[6] >> 4 != 4 {
            return Err(ServiceIdError::InvalidVersion);
        }
        if bytes[8] & 0xc0 != 0x80 {
            return Err(ServiceIdError::InvalidVariant);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; SERVICE_ID_BYTES] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; SERVICE_ID_BYTES] {
        self.0
    }
}

impl fmt::Display for ServiceId {
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

impl fmt::Debug for ServiceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ServiceId({self})")
    }
}

/// Nonzero service role identifier.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoleId(NonZeroU32);

impl RoleId {
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Monotonically increasing, nonzero provider incarnation.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderGeneration(NonZeroU64);

impl ProviderGeneration {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A service and one independently authorized role within that service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteKey {
    service: ServiceId,
    role: RoleId,
}

impl RouteKey {
    pub const fn new(service: ServiceId, role: RoleId) -> Self {
        Self { service, role }
    }

    pub const fn service(self) -> ServiceId {
        self.service
    }

    pub const fn role(self) -> RoleId {
        self.role
    }
}
