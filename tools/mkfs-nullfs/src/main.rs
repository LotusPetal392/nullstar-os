use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use nullfs_blockdev::FileBlockDevice;
use nullfs_format::{BLOCK_SIZE, FIRST_DESCRIPTOR_BLOCK, SUPERBLOCK_BLOCK};
use nullfs_testkit::ImageBuilder;

#[derive(Debug)]
struct Options {
    path: PathBuf,
    label: String,
    size: Option<u64>,
    force: bool,
}

fn main() -> ExitCode {
    match parse_options().and_then(format_volume) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mkfs-nullfs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_options() -> Result<Options, String> {
    let mut arguments = env::args().skip(1);
    let mut path = None;
    let mut label = String::new();
    let mut size = None;
    let mut force = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--force" => force = true,
            "--label" => {
                label = arguments
                    .next()
                    .ok_or_else(|| String::from("--label requires a value"))?;
            }
            "--size" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--size requires a value"))?;
                size = Some(parse_size(&value)?);
            }
            "-h" | "--help" => return Err(usage()),
            _ if argument.starts_with('-') => {
                return Err(format!("unknown option `{argument}`\n{}", usage()));
            }
            _ if path.is_none() => path = Some(PathBuf::from(argument)),
            _ => return Err(format!("unexpected argument `{argument}`\n{}", usage())),
        }
    }
    let path = path.ok_or_else(usage)?;
    Ok(Options {
        path,
        label,
        size,
        force,
    })
}

fn usage() -> String {
    String::from("usage: mkfs-nullfs [--force] [--label LABEL] [--size SIZE] TARGET")
}

fn parse_size(value: &str) -> Result<u64, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("KiB") {
        (number, 1024_u64)
    } else if let Some(number) = value.strip_suffix("MiB") {
        (number, 1024_u64 * 1024)
    } else if let Some(number) = value.strip_suffix("GiB") {
        (number, 1024_u64 * 1024 * 1024)
    } else {
        (value, 1)
    };
    number
        .parse::<u64>()
        .map_err(|_| format!("invalid size `{value}`"))?
        .checked_mul(multiplier)
        .ok_or_else(|| format!("size `{value}` overflows u64"))
}

fn format_volume(options: Options) -> Result<(), String> {
    if options.path.exists() && !options.force && target_metadata_present(&options.path)? {
        return Err(String::from(
            "target metadata area is not blank; pass --force to overwrite it",
        ));
    }
    if let Some(size) = options.size {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&options.path)
            .map_err(|error| format!("could not create target: {error}"))?;
        file.set_len(size)
            .map_err(|error| format!("could not resize target: {error}"))?;
    } else if !options.path.exists() {
        return Err(String::from(
            "target does not exist; create it first or pass --size",
        ));
    }

    let device_bytes = fs::metadata(&options.path)
        .map_err(|error| format!("could not inspect target: {error}"))?
        .len();
    let uuid = generate_uuid(&options.path, device_bytes);
    let device = FileBlockDevice::open(&options.path, BLOCK_SIZE, true)
        .map_err(|error| error.to_string())?;
    let builder =
        ImageBuilder::new(device, uuid, &options.label).map_err(|error| error.to_string())?;
    let superblock = builder.superblock().clone();
    builder.finish().map_err(|error| error.to_string())?;

    println!(
        "Formatted {} as NullFS 1.2 (Phase 3)",
        options.path.display()
    );
    println!("UUID:  {}", format_uuid(uuid));
    println!("Label: {}", superblock.label());
    println!(
        "Blocks: {} × {} bytes",
        superblock.capacity_blocks, BLOCK_SIZE
    );
    println!(
        "Metadata: primary={}, descriptors={}, backup={}, state={}, journal={}..{}",
        SUPERBLOCK_BLOCK,
        FIRST_DESCRIPTOR_BLOCK,
        superblock.backup_superblock_block,
        superblock.filesystem_state_block,
        superblock.journal_first_block,
        superblock.first_allocatable_block
    );
    Ok(())
}

fn target_metadata_present(path: &Path) -> Result<bool, String> {
    let mut file = File::open(path).map_err(|error| format!("could not open target: {error}"))?;
    let target_length = file
        .metadata()
        .map_err(|error| format!("could not inspect target: {error}"))?
        .len();
    let checked_bytes = (FIRST_DESCRIPTOR_BLOCK + 1)
        .checked_mul(BLOCK_SIZE as u64)
        .ok_or_else(|| String::from("metadata inspection length overflowed"))?
        .min(target_length);
    let checked_bytes = usize::try_from(checked_bytes)
        .map_err(|_| String::from("metadata inspection length is too large"))?;
    let mut bytes = vec![0; checked_bytes];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("could not read target: {error}"))?;
    Ok(bytes.iter().any(|byte| *byte != 0))
}

fn generate_uuid(path: &Path, device_bytes: u64) -> [u8; 16] {
    let mut uuid = [0; 16];
    if File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut uuid))
        .is_err()
    {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let mut state = nanos ^ u128::from(device_bytes) ^ u128::from(std::process::id());
        for byte in path.as_os_str().as_encoded_bytes() {
            state ^= u128::from(*byte);
            state = state.rotate_left(11).wrapping_mul(0x100_0000_01b3);
        }
        uuid = state.to_le_bytes();
    }
    uuid[6] = (uuid[6] & 0x0f) | 0x40;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid
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

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::{Read, Seek, SeekFrom},
    };

    use nullfs_format::{
        BLOCK_SIZE, FIRST_DESCRIPTOR_BLOCK, MountMode, RESERVED_BOOT_BYTES, SUPERBLOCK_BLOCK,
        Superblock,
    };

    use super::{Options, format_volume, parse_size};

    #[test]
    fn parses_binary_size_suffixes() {
        assert_eq!(parse_size("4096").expect("bytes"), 4096);
        assert_eq!(parse_size("64KiB").expect("KiB"), 64 * 1024);
        assert_eq!(parse_size("64MiB").expect("MiB"), 64 * 1024 * 1024);
        assert!(parse_size("many").is_err());
    }

    #[test]
    fn formats_mountable_phase_three_layout_and_refuses_overwrite() {
        let path =
            std::env::temp_dir().join(format!("nullfs-mkfs-test-{}.img", std::process::id()));
        let image_bytes = 64 * 1024 * 1024;
        let _ = fs::remove_file(&path);
        format_volume(Options {
            path: path.clone(),
            label: String::from("Phase3"),
            size: Some(image_bytes),
            force: false,
        })
        .expect("format image");

        let mut file = File::open(&path).expect("open image");
        let mut boot_area = vec![0; RESERVED_BOOT_BYTES as usize];
        file.read_exact(&mut boot_area).expect("read boot area");
        assert!(boot_area.iter().all(|byte| *byte == 0));

        file.seek(SeekFrom::Start(SUPERBLOCK_BLOCK * BLOCK_SIZE as u64))
            .expect("seek superblock");
        let mut superblock_bytes = [0; BLOCK_SIZE];
        file.read_exact(&mut superblock_bytes)
            .expect("read superblock");
        let superblock =
            Superblock::decode(&superblock_bytes, Some(image_bytes), MountMode::ReadWrite)
                .expect("decode superblock");
        assert_eq!(superblock.label(), "Phase3");
        assert!(superblock.phase3_enabled());
        assert_eq!(superblock.first_descriptor_block, FIRST_DESCRIPTOR_BLOCK);

        file.seek(SeekFrom::Start(FIRST_DESCRIPTOR_BLOCK * BLOCK_SIZE as u64))
            .expect("seek descriptor reservation");
        let mut descriptor_block = [1; BLOCK_SIZE];
        file.read_exact(&mut descriptor_block)
            .expect("read descriptor reservation");
        assert!(descriptor_block.iter().any(|byte| *byte != 0));

        let device = nullfs_blockdev::FileBlockDevice::open(&path, BLOCK_SIZE, true)
            .expect("open block device");
        let mut filesystem =
            nullfs_core::Filesystem::mount_read_write(device).expect("mount Phase 3 image");
        assert_eq!(
            filesystem
                .attributes(filesystem.root())
                .expect("root attributes")
                .kind,
            nullfs_format::NodeKind::Directory
        );
        filesystem.unmount().expect("clean unmount");

        assert!(
            format_volume(Options {
                path: path.clone(),
                label: String::new(),
                size: None,
                force: false,
            })
            .is_err()
        );
        fs::remove_file(path).expect("remove test image");
    }
}
