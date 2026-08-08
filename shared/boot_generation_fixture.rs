// Deterministic boot-generation artifacts and selection records shared by image construction and
// the freestanding acceptance probe.

use boot_generation::{
    GenerationId, Health, RetainedGeneration, Selection, SelectionSequence, Slot,
};
use nullfs_format::crc32c;

pub const CANONICAL_SELECTION_PATH: &str = "/System/boot/selection";
pub const GENERATION_1_PATH: &str = "/System/boot/generations/1/kernel";
pub const GENERATION_1_MANIFEST_PATH: &str = "/System/boot/generations/1/manifest";
pub const GENERATION_2_PATH: &str = "/System/boot/generations/2/kernel";
pub const GENERATION_2_MANIFEST_PATH: &str = "/System/boot/generations/2/manifest";

pub const FIRMWARE_SLOT_0_PATH: &[u8] = b"/BOOT0.BIN";
pub const FIRMWARE_SLOT_1_PATH: &[u8] = b"/BOOT1.BIN";
pub const FIRMWARE_SELECTION_PATH: &[u8] = b"/BOOTSEL.BIN";

pub const GENERATION_1_KERNEL: &[u8] = b"NullStar boot generation 1 kernel artifact.\n";
pub const GENERATION_2_KERNEL: &[u8] = b"NullStar boot generation 2 kernel artifact.\n";
pub const GENERATION_1_MANIFEST: &[u8] =
    b"nullstar-boot-generation-v1\ngeneration=1\nslot=0\nartifact=kernel\n";
pub const GENERATION_2_MANIFEST: &[u8] =
    b"nullstar-boot-generation-v1\ngeneration=2\nslot=1\nartifact=kernel\n";

pub fn generation_1(health: Health) -> RetainedGeneration {
    RetainedGeneration::new(
        GenerationId::new(1).expect("generation 1 is nonzero"),
        Slot::Zero,
        health,
        crc32c(GENERATION_1_KERNEL),
    )
}

pub fn generation_2(health: Health) -> RetainedGeneration {
    RetainedGeneration::new(
        GenerationId::new(2).expect("generation 2 is nonzero"),
        Slot::One,
        health,
        crc32c(GENERATION_2_KERNEL),
    )
}

pub fn initial_selection() -> Selection {
    Selection::new(
        SelectionSequence::new(1).expect("initial selection sequence is nonzero"),
        generation_1(Health::Healthy),
        None,
    )
    .expect("initial selection is canonical")
}

pub fn staged_selection() -> Selection {
    Selection::new(
        SelectionSequence::new(2).expect("staged selection sequence is nonzero"),
        generation_2(Health::Pending),
        Some(generation_1(Health::Healthy)),
    )
    .expect("staged selection is canonical")
}

pub fn rollback_selection() -> Selection {
    Selection::new(
        SelectionSequence::new(3).expect("rollback selection sequence is nonzero"),
        generation_1(Health::Healthy),
        Some(generation_2(Health::Failed)),
    )
    .expect("rollback selection is canonical")
}
