//! NullFS feature compatibility policy.

use crate::{Error, MountMode};

pub const INCOMPAT_PHASE2_CORE: u64 = 1;
/// Phase 3 fixed-geometry writable redo journal.
pub const INCOMPAT_PHASE3_WRITABLE_REDO: u64 = 1 << 1;

pub const SUPPORTED_COMPATIBLE: u64 = 0;
pub const SUPPORTED_READ_ONLY_COMPATIBLE: u64 = 0;
pub const SUPPORTED_INCOMPATIBLE: u64 = INCOMPAT_PHASE2_CORE | INCOMPAT_PHASE3_WRITABLE_REDO;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Features {
    pub compatible: u64,
    pub read_only_compatible: u64,
    pub incompatible: u64,
}

impl Features {
    pub fn validate(self, mode: MountMode) -> Result<(), Error> {
        let unknown_incompatible = self.incompatible & !SUPPORTED_INCOMPATIBLE;
        if unknown_incompatible != 0 {
            return Err(Error::UnsupportedIncompatibleFeatures(unknown_incompatible));
        }
        let unknown_read_only = self.read_only_compatible & !SUPPORTED_READ_ONLY_COMPATIBLE;
        if mode == MountMode::ReadWrite && unknown_read_only != 0 {
            return Err(Error::ReadOnlyFeaturesRequired(unknown_read_only));
        }
        Ok(())
    }
}
