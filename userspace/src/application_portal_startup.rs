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
    ///
    /// `#[inline(never)]` is required: this method calls into `ApplicationPortalTransport::mint`, which
    /// constructs a 64-slot client binding array on its own stack frame as part of returning the full
    /// transport by value. Inlined into build_service that peak would merge with build_service's ~149 KiB
    /// construction temporaries and overflow the 256 KiB userspace stack during startup (vector 14 inside
    /// ApplicationPortalTransport::mint, rip in release builds mapping to mint itself). As a separate call
    /// its small frame and mint's isolated peak occur sequentially rather than coexisting with build_service.
    #[inline(never)]
    pub fn start_transport(&self) -> Result<
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

pub const APPLICATION_PORTAL_SERVICE_STARTUP_BYTES: usize = 104;

/// Complete live-service configuration: a private state file and a separately selected browse root.
/// All identities come from the authenticated parent, and must belong to the same pinned volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationPortalServiceStartup {
    pub portal: ApplicationPortalStartup,
    pub state_file: crate::application_permission::ApplicationResourceIdentity,
    pub picker_root: crate::application_permission::ApplicationResourceIdentity,
}

impl ApplicationPortalServiceStartup {
    pub fn new(
        portal: ApplicationPortalStartup,
        state_file: crate::application_permission::ApplicationResourceIdentity,
        picker_root: crate::application_permission::ApplicationResourceIdentity,
    ) -> Option<Self> {
        use crate::application_permission::ApplicationResourceKind;
        (state_file.kind() == ApplicationResourceKind::File
            && picker_root.kind() == ApplicationResourceKind::Directory
            && state_file.filesystem_uuid() == picker_root.filesystem_uuid())
        .then_some(Self {
            portal,
            state_file,
            picker_root,
        })
    }

    pub fn encode(self) -> [u8; APPLICATION_PORTAL_SERVICE_STARTUP_BYTES] {
        let mut bytes = [0; APPLICATION_PORTAL_SERVICE_STARTUP_BYTES];
        bytes[..48].copy_from_slice(&self.portal.encode());
        bytes[48..64].copy_from_slice(&self.state_file.filesystem_uuid());
        for (offset, value) in [
            (64, self.state_file.object_id()),
            (72, self.state_file.object_generation()),
            (80, self.picker_root.object_id()),
            (88, self.picker_root.object_generation()),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        let checksum = crc32c(&bytes[..96]);
        bytes[96..100].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        use crate::application_permission::{ApplicationResourceIdentity, ApplicationResourceKind};
        if bytes.len() != APPLICATION_PORTAL_SERVICE_STARTUP_BYTES
            || bytes[100..] != [0; 4]
            || crc32c(&bytes[..96]) != u32::from_le_bytes(bytes[96..100].try_into().ok()?)
        {
            return None;
        }
        let portal = ApplicationPortalStartup::decode(&bytes[..48]).ok()?;
        let uuid = bytes[48..64].try_into().ok()?;
        Self::new(
            portal,
            ApplicationResourceIdentity::new(
                uuid,
                read_u64(bytes, 64),
                read_u64(bytes, 72),
                ApplicationResourceKind::File,
            )?,
            ApplicationResourceIdentity::new(
                uuid,
                read_u64(bytes, 80),
                read_u64(bytes, 88),
                ApplicationResourceKind::Directory,
            )?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_startup_checksums_storage_and_browse_identities() {
        use crate::application_permission::{ApplicationResourceIdentity, ApplicationResourceKind};
        let portal = ApplicationPortalStartup::new(10, 20, 7, 30).unwrap();
        let file =
            ApplicationResourceIdentity::new([1; 16], 2, 3, ApplicationResourceKind::File).unwrap();
        let directory =
            ApplicationResourceIdentity::new([1; 16], 4, 5, ApplicationResourceKind::Directory)
                .unwrap();
        let startup = ApplicationPortalServiceStartup::new(portal, file, directory).unwrap();
        let bytes = startup.encode();
        assert_eq!(
            ApplicationPortalServiceStartup::decode(&bytes),
            Some(startup)
        );
        for offset in [48, 64, 72, 80, 88, 96, 100] {
            let mut corrupt = bytes;
            corrupt[offset] ^= 1;
            assert!(ApplicationPortalServiceStartup::decode(&corrupt).is_none());
        }
        let foreign =
            ApplicationResourceIdentity::new([2; 16], 4, 5, ApplicationResourceKind::Directory)
                .unwrap();
        assert!(ApplicationPortalServiceStartup::new(portal, file, foreign).is_none());
        assert!(ApplicationPortalServiceStartup::new(portal, directory, file).is_none());
    }

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
