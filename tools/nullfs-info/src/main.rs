use std::{env, fs, path::PathBuf, process::ExitCode};

use nullfs_blockdev::{BlockDevice, FileBlockDevice};
use nullfs_format::{BLOCK_SIZE, MountMode, SUPERBLOCK_BLOCK, Superblock, VolumeState};

fn main() -> ExitCode {
    match inspect() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nullfs-info: {error}");
            ExitCode::FAILURE
        }
    }
}

fn inspect() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| String::from("usage: nullfs-info TARGET"))?;
    if let Some(argument) = arguments.next() {
        return Err(format!("unexpected argument `{argument}`"));
    }
    let device_bytes = fs::metadata(&path)
        .map_err(|error| format!("could not inspect target: {error}"))?
        .len();
    let mut device =
        FileBlockDevice::open(&path, BLOCK_SIZE, false).map_err(|error| error.to_string())?;
    let mut bytes = [0; BLOCK_SIZE];
    device
        .read_blocks(SUPERBLOCK_BLOCK, &mut bytes)
        .map_err(|error| error.to_string())?;
    let superblock = Superblock::decode(&bytes, Some(device_bytes), MountMode::ReadOnly)
        .map_err(|error| error.to_string())?;

    println!("Filesystem:       NullFS");
    println!(
        "Format:           {}.{}",
        superblock.format_major, superblock.format_minor
    );
    println!(
        "Layout:           {}",
        if superblock.phase3_enabled() {
            "Phase 3 (writable redo journal)"
        } else if superblock.phase2_enabled() {
            "Phase 2 (read-only core records)"
        } else {
            "Phase 1 (format foundation only)"
        }
    );
    println!(
        "UUID:             {}",
        format_uuid(superblock.filesystem_uuid)
    );
    println!("Label:            {}", superblock.label());
    println!("Block size:       {}", BLOCK_SIZE);
    println!("Capacity blocks:  {}", superblock.capacity_blocks);
    println!(
        "Capacity bytes:   {}",
        superblock.capacity_blocks.saturating_mul(BLOCK_SIZE as u64)
    );
    println!(
        "State:            {}",
        match superblock.state {
            VolumeState::Clean => "clean",
            VolumeState::Dirty => "dirty",
        }
    );
    println!(
        "Features:         compat={:#018x} ro_compat={:#018x} incompat={:#018x}",
        superblock.features.compatible,
        superblock.features.read_only_compatible,
        superblock.features.incompatible
    );
    println!(
        "Allocation groups: {} × up to {} blocks",
        superblock.allocation_group_count, superblock.allocation_group_blocks
    );
    println!("Descriptor start:  {}", superblock.first_descriptor_block);
    println!("First allocatable: {}", superblock.first_allocatable_block);
    if superblock.phase3_enabled() {
        println!("Backup superblock: {}", superblock.backup_superblock_block);
        println!("Filesystem state:  {}", superblock.filesystem_state_block);
        println!(
            "Journal:           {}..{} ({} updates)",
            superblock.journal_first_block,
            superblock.journal_first_block + u64::from(superblock.journal_block_count),
            superblock.journal_max_updates
        );
    }
    Ok(())
}

fn format_uuid(uuid: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        uuid[0],
        uuid[1],
        uuid[2],
        uuid[3],
        uuid[4],
        uuid[5],
        uuid[6],
        uuid[7],
        uuid[8],
        uuid[9],
        uuid[10],
        uuid[11],
        uuid[12],
        uuid[13],
        uuid[14],
        uuid[15]
    )
}
