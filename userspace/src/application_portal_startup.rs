//! Authenticated startup configuration for the application portal service.
//!
//! The process IDs in this record are descriptive configuration supplied by the trusted launcher.
//! They become useful only because the portal transport compares them with kernel-stamped sender
//! identities on every manager and compositor operation.

use nullfs_format::crc32c;

use crate::application_portal_transport::{
    ApplicationPortalClientSource, ApplicationPortalGestureSource, ApplicationPortalTransport,
    ApplicationPortalTransportCreateError,
};
use crate::process_start::StartupSectionId;

pub const APPLICATION_PORTAL_STARTUP_MAGIC: [u8; 4] = *b"NSPS";
pub const APPLICATION_PORTAL_STARTUP_VERSION: u16 = 1;
pub const APPLICATION_PORTAL_STARTUP_BYTES: usize = 48;
pub const APPLICATION_PORTAL_STARTUP_SECTION: StartupSectionId =
    StartupSectionId::APPLICATION_PORTAL;
const CHECKSUM_OFFSET: usize = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationPortalStartup {
    application_manager_process_id: u64,
    trusted_compositor_process_id: u64,
    manager_generation: u64,
    session_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalStartupError {
    InvalidIdentity,
    AliasedRoles,
    Length,
    Magic,
    Version,
    Reserved,
    Checksum,
}

impl ApplicationPortalStartup {
    pub const fn new(
        application_manager_process_id: u64,
        trusted_compositor_process_id: u64,
        manager_generation: u64,
        session_id: u64,
    ) -> Result<Self, ApplicationPortalStartupError> {
        if application_manager_process_id == 0
            || trusted_compositor_process_id == 0
            || manager_generation == 0
            || session_id == 0
        {
            return Err(ApplicationPortalStartupError::InvalidIdentity);
        }
        if application_manager_process_id == trusted_compositor_process_id {
            return Err(ApplicationPortalStartupError::AliasedRoles);
        }
        Ok(Self {
            application_manager_process_id,
            trusted_compositor_process_id,
            manager_generation,
            session_id,
        })
    }

    pub const fn application_manager_process_id(self) -> u64 {
        self.application_manager_process_id
    }

    pub const fn trusted_compositor_process_id(self) -> u64 {
        self.trusted_compositor_process_id
    }

    pub const fn manager_generation(self) -> u64 {
        self.manager_generation
    }

    pub const fn session_id(self) -> u64 {
        self.session_id
    }

    pub fn encode(self) -> [u8; APPLICATION_PORTAL_STARTUP_BYTES] {
        let mut bytes = [0; APPLICATION_PORTAL_STARTUP_BYTES];
        bytes[..4].copy_from_slice(&APPLICATION_PORTAL_STARTUP_MAGIC);
        bytes[4..6].copy_from_slice(&APPLICATION_PORTAL_STARTUP_VERSION.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.application_manager_process_id.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.trusted_compositor_process_id.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.manager_generation.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.session_id.to_le_bytes());
        let checksum = crc32c(&bytes[..CHECKSUM_OFFSET]);
        bytes[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ApplicationPortalStartupError> {
        if bytes.len() != APPLICATION_PORTAL_STARTUP_BYTES {
            return Err(ApplicationPortalStartupError::Length);
        }
        if bytes[..4] != APPLICATION_PORTAL_STARTUP_MAGIC {
            return Err(ApplicationPortalStartupError::Magic);
        }
        if u16::from_le_bytes([bytes[4], bytes[5]]) != APPLICATION_PORTAL_STARTUP_VERSION {
            return Err(ApplicationPortalStartupError::Version);
        }
        if bytes[6..8] != [0; 2] || bytes[40..44] != [0; 4] {
            return Err(ApplicationPortalStartupError::Reserved);
        }
        let expected = u32::from_le_bytes(bytes[CHECKSUM_OFFSET..].try_into().unwrap());
        if crc32c(&bytes[..CHECKSUM_OFFSET]) != expected {
            return Err(ApplicationPortalStartupError::Checksum);
        }
        Self::new(
            read_u64(bytes, 8),
            read_u64(bytes, 16),
            read_u64(bytes, 24),
            read_u64(bytes, 32),
        )
    }

    /// Mints the two capability-separated ingresses using only authenticated startup identities.
    pub fn start_transport(
        self,
    ) -> Result<
        (
            ApplicationPortalTransport,
            ApplicationPortalClientSource,
            ApplicationPortalGestureSource,
        ),
        ApplicationPortalTransportCreateError,
    > {
        ApplicationPortalTransport::mint(
            self.application_manager_process_id,
            self.trusted_compositor_process_id,
        )
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_record_is_canonical_and_checksummed() {
        let record = ApplicationPortalStartup::new(10, 20, 7, 30).unwrap();
        let encoded = record.encode();
        assert_eq!(ApplicationPortalStartup::decode(&encoded), Ok(record));

        let mut corrupt = encoded;
        corrupt[24] ^= 1;
        assert_eq!(
            ApplicationPortalStartup::decode(&corrupt),
            Err(ApplicationPortalStartupError::Checksum)
        );
        assert_eq!(
            ApplicationPortalStartup::new(10, 10, 7, 30),
            Err(ApplicationPortalStartupError::AliasedRoles)
        );
    }
}
