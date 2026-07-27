use std::{
    env,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::ExitCode,
};

use nullfs_blockdev::FileBlockDevice;
use nullfs_core::Filesystem;
use nullfs_format::BLOCK_SIZE;
use nullfs_testkit::ImageBuilder;

const IMAGE_UUID: [u8; 16] = [
    0x4e, 0x75, 0x6c, 0x6c, 0x53, 0x74, 0x41, 0x72, 0x80, 0x02, 0, 0, 0, 0, 0, 1,
];

struct CreateOptions {
    image: PathBuf,
    source: PathBuf,
    size: u64,
    label: String,
    force: bool,
}

fn main() -> ExitCode {
    match parse_options().and_then(create_image) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nullfs-image: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_options() -> Result<CreateOptions, String> {
    let mut arguments = env::args().skip(1);
    if arguments.next().as_deref() != Some("create") {
        return Err(usage());
    }
    let mut image = None;
    let mut source = None;
    let mut size = None;
    let mut label = String::new();
    let mut force = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--source" => {
                source = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| String::from("--source requires a path"))?,
                ));
            }
            "--size" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| String::from("--size requires a value"))?;
                size = Some(parse_size(&value)?);
            }
            "--label" => {
                label = arguments
                    .next()
                    .ok_or_else(|| String::from("--label requires a value"))?;
            }
            "--force" => force = true,
            "-h" | "--help" => return Err(usage()),
            _ if argument.starts_with('-') => return Err(format!("unknown option `{argument}`")),
            _ if image.is_none() => image = Some(PathBuf::from(argument)),
            _ => return Err(format!("unexpected argument `{argument}`")),
        }
    }
    Ok(CreateOptions {
        image: image.ok_or_else(usage)?,
        source: source.ok_or_else(|| String::from("--source is required"))?,
        size: size.ok_or_else(|| String::from("--size is required"))?,
        label,
        force,
    })
}

fn usage() -> String {
    String::from(
        "usage: nullfs-image create --size SIZE --source ROOT [--label LABEL] [--force] IMAGE",
    )
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

fn create_image(options: CreateOptions) -> Result<(), String> {
    if !options.source.is_dir() {
        return Err(format!(
            "source `{}` is not a directory",
            options.source.display()
        ));
    }
    if options.image.exists() && !options.force {
        return Err(format!(
            "image `{}` already exists; pass --force to replace it",
            options.image.display()
        ));
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&options.image)
        .map_err(|error| format!("could not create image: {error}"))?;
    file.set_len(options.size)
        .map_err(|error| format!("could not resize image: {error}"))?;
    drop(file);

    let device = FileBlockDevice::open(&options.image, BLOCK_SIZE, true)
        .map_err(|error| error.to_string())?;
    let mut builder =
        ImageBuilder::new(device, IMAGE_UUID, &options.label).map_err(|error| error.to_string())?;
    populate_directory(&mut builder, 1, &options.source)?;
    let device = builder.finish().map_err(|error| error.to_string())?;
    Filesystem::mount(device)
        .map_err(|error| format!("created image failed validation: {error}"))?;
    println!(
        "Created deterministic NullFS 1.2 image {} from {}",
        options.image.display(),
        options.source.display()
    );
    Ok(())
}

fn populate_directory(
    builder: &mut ImageBuilder<FileBlockDevice>,
    parent: u64,
    source: &Path,
) -> Result<(), String> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not read {}: {error}", source.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("non-UTF-8 name under {}", source.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            let inode = builder
                .create_directory(parent, &name, 0o755)
                .map_err(|error| format!("could not add {}: {error}", path.display()))?;
            populate_directory(builder, inode, &path)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            builder
                .create_file(parent, &name, &bytes, 0o644)
                .map_err(|error| format!("could not add {}: {error}", path.display()))?;
        } else {
            return Err(format!(
                "unsupported symlink or special file `{}`",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("64MiB").expect("size"), 64 * 1024 * 1024);
        assert!(parse_size("invalid").is_err());
    }
}
