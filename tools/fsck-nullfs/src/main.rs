use std::{
    env,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use nullfs_blockdev::FileBlockDevice;
use nullfs_core::{Error as FilesystemError, Filesystem};
use nullfs_format::{BLOCK_SIZE, VolumeState};

const USAGE: &str = "usage: fsck-nullfs [--json] IMAGE";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    json: bool,
    image: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Error,
    RecoveryRequired,
}

#[derive(Debug, PartialEq, Eq)]
struct CheckFailure {
    kind: FailureKind,
    message: String,
}

impl CheckFailure {
    fn error(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Error,
            message: message.into(),
        }
    }

    fn recovery_required(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::RecoveryRequired,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Statistics {
    total_data_blocks: u64,
    free_data_blocks: u64,
    used_data_blocks: u64,
    total_inodes: u64,
    free_inodes: u64,
    used_inodes: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct CheckReport {
    format_major: u16,
    format_minor: u16,
    label: String,
    volume_state: &'static str,
    capacity_blocks: u64,
    capacity_bytes: u64,
    statistics: Option<Statistics>,
}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let json = arguments
        .iter()
        .any(|argument| argument == OsStr::new("--json"));

    let result = parse_options(arguments)
        .and_then(|options| check_image(&options.image).map(|report| (options.json, report)));

    match result {
        Ok((json, report)) => {
            if json {
                println!("{}", render_json_report(&report));
            } else {
                println!("{}", render_text_report(&report));
            }
            ExitCode::SUCCESS
        }
        Err(failure) => {
            if json {
                println!("{}", render_json_failure(&failure));
            } else {
                eprintln!("fsck-nullfs: {}", failure.message);
            }
            ExitCode::FAILURE
        }
    }
}

fn parse_options(arguments: impl IntoIterator<Item = OsString>) -> Result<Options, CheckFailure> {
    let mut json = false;
    let mut image = None;

    for argument in arguments {
        if argument == OsStr::new("--json") {
            if json {
                return Err(CheckFailure::error(
                    "option `--json` was provided more than once",
                ));
            }
            if image.is_some() {
                return Err(CheckFailure::error(format!(
                    "option `--json` must precede IMAGE; {USAGE}"
                )));
            }
            json = true;
        } else if argument == OsStr::new("-h") || argument == OsStr::new("--help") {
            return Err(CheckFailure::error(USAGE));
        } else if argument.to_string_lossy().starts_with('-') {
            return Err(CheckFailure::error(format!(
                "unknown option `{}`; {USAGE}",
                argument.to_string_lossy()
            )));
        } else if image.is_none() {
            image = Some(PathBuf::from(argument));
        } else {
            return Err(CheckFailure::error(format!(
                "unexpected argument `{}`; {USAGE}",
                argument.to_string_lossy()
            )));
        }
    }

    Ok(Options {
        json,
        image: image.ok_or_else(|| CheckFailure::error(USAGE))?,
    })
}

fn check_image(path: &Path) -> Result<CheckReport, CheckFailure> {
    let device = FileBlockDevice::open(path, BLOCK_SIZE, false).map_err(|error| {
        CheckFailure::error(format!(
            "could not open image `{}` read-only: {error}",
            path.display()
        ))
    })?;
    let filesystem = Filesystem::mount(device).map_err(|error| mount_failure(path, error))?;
    let superblock = filesystem.superblock();

    let capacity_bytes = superblock
        .capacity_blocks
        .checked_mul(BLOCK_SIZE as u64)
        .ok_or_else(|| CheckFailure::error("validated filesystem capacity overflows u64"))?;
    let statistics = if superblock.phase3_enabled() {
        let statistics = filesystem.statistics().map_err(|error| {
            CheckFailure::error(format!(
                "image `{}` has invalid filesystem statistics: {error}",
                path.display()
            ))
        })?;
        Some(Statistics {
            total_data_blocks: statistics.total_data_blocks,
            free_data_blocks: statistics.free_data_blocks,
            used_data_blocks: statistics
                .total_data_blocks
                .checked_sub(statistics.free_data_blocks)
                .ok_or_else(|| CheckFailure::error("free data-block count exceeds total"))?,
            total_inodes: statistics.total_inodes,
            free_inodes: statistics.free_inodes,
            used_inodes: statistics
                .total_inodes
                .checked_sub(statistics.free_inodes)
                .ok_or_else(|| CheckFailure::error("free inode count exceeds total"))?,
        })
    } else {
        None
    };

    Ok(CheckReport {
        format_major: superblock.format_major,
        format_minor: superblock.format_minor,
        label: superblock.label().to_owned(),
        volume_state: match superblock.state {
            VolumeState::Clean => "clean",
            VolumeState::Dirty => "dirty",
        },
        capacity_blocks: superblock.capacity_blocks,
        capacity_bytes,
        statistics,
    })
}

fn mount_failure(path: &Path, error: FilesystemError) -> CheckFailure {
    if error == FilesystemError::RecoveryRequired {
        CheckFailure::recovery_required(format!(
            "recovery required for image `{}`: persistent orphaned inodes must be reclaimed; repair is not implemented",
            path.display()
        ))
    } else {
        CheckFailure::error(format!(
            "image `{}` failed NullFS validation: {error}",
            path.display()
        ))
    }
}

fn render_text_report(report: &CheckReport) -> String {
    let mut output = String::new();
    writeln!(output, "Status:            clean").unwrap();
    writeln!(
        output,
        "Format:            NullFS {}.{} (valid)",
        report.format_major, report.format_minor
    )
    .unwrap();
    writeln!(output, "Label:             {}", report.label).unwrap();
    writeln!(output, "Volume state:      {}", report.volume_state).unwrap();
    writeln!(output, "Block size:        {BLOCK_SIZE} bytes").unwrap();
    writeln!(
        output,
        "Capacity:          {} blocks ({} bytes)",
        report.capacity_blocks, report.capacity_bytes
    )
    .unwrap();
    if let Some(statistics) = report.statistics {
        writeln!(
            output,
            "Data blocks:       total={} used={} free={}",
            statistics.total_data_blocks, statistics.used_data_blocks, statistics.free_data_blocks
        )
        .unwrap();
        write!(
            output,
            "Inodes:            total={} used={} free={}",
            statistics.total_inodes, statistics.used_inodes, statistics.free_inodes
        )
        .unwrap();
    } else {
        write!(
            output,
            "Statistics:        unavailable for pre-Phase 3 format"
        )
        .unwrap();
    }
    output
}

fn render_json_report(report: &CheckReport) -> String {
    let statistics = match report.statistics {
        Some(statistics) => format!(
            "{{\"data_blocks\":{{\"total\":{},\"used\":{},\"free\":{}}},\"inodes\":{{\"total\":{},\"used\":{},\"free\":{}}}}}",
            statistics.total_data_blocks,
            statistics.used_data_blocks,
            statistics.free_data_blocks,
            statistics.total_inodes,
            statistics.used_inodes,
            statistics.free_inodes
        ),
        None => String::from("null"),
    };
    format!(
        "{{\"status\":\"clean\",\"valid\":true,\"filesystem\":\"NullFS\",\"format\":{{\"major\":{},\"minor\":{}}},\"label\":{},\"volume_state\":\"{}\",\"block_size\":{},\"capacity\":{{\"blocks\":{},\"bytes\":{}}},\"statistics\":{}}}",
        report.format_major,
        report.format_minor,
        json_string(&report.label),
        report.volume_state,
        BLOCK_SIZE,
        report.capacity_blocks,
        report.capacity_bytes,
        statistics
    )
}

fn render_json_failure(failure: &CheckFailure) -> String {
    let status = match failure.kind {
        FailureKind::Error => "error",
        FailureKind::RecoveryRequired => "recovery-required",
    };
    format!(
        "{{\"status\":\"{status}\",\"valid\":false,\"error\":{}}}",
        json_string(&failure.message)
    )
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", character as u32).unwrap();
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        sync::atomic::{AtomicU64, Ordering},
    };

    use nullfs_blockdev::FileBlockDevice;
    use nullfs_testkit::ImageBuilder;

    use super::*;

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);
    const TEST_IMAGE_BLOCKS: u64 = 4096;

    struct TempImage(PathBuf);

    impl TempImage {
        fn new() -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "fsck-nullfs-test-{}-{sequence}.img",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .expect("create temporary image");
            file.set_len(TEST_IMAGE_BLOCKS * BLOCK_SIZE as u64)
                .expect("size temporary image");
            drop(file);
            Self(path)
        }
    }

    impl Drop for TempImage {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn sample_report() -> CheckReport {
        CheckReport {
            format_major: 1,
            format_minor: 2,
            label: String::from("test\"label"),
            volume_state: "clean",
            capacity_blocks: 4096,
            capacity_bytes: 16_777_216,
            statistics: Some(Statistics {
                total_data_blocks: 3900,
                free_data_blocks: 3898,
                used_data_blocks: 2,
                total_inodes: 128,
                free_inodes: 127,
                used_inodes: 1,
            }),
        }
    }

    #[test]
    fn parses_documented_arguments() {
        assert_eq!(
            parse_options(arguments(&["disk.img"])).unwrap(),
            Options {
                json: false,
                image: PathBuf::from("disk.img")
            }
        );
        assert_eq!(
            parse_options(arguments(&["--json", "disk.img"])).unwrap(),
            Options {
                json: true,
                image: PathBuf::from("disk.img")
            }
        );
    }

    #[test]
    fn rejects_missing_extra_and_unknown_arguments() {
        assert!(parse_options(arguments(&[])).is_err());
        assert!(parse_options(arguments(&["one.img", "two.img"])).is_err());
        assert!(parse_options(arguments(&["--unknown", "disk.img"])).is_err());
        assert!(parse_options(arguments(&["disk.img", "--json"])).is_err());
        assert!(parse_options(arguments(&["--json", "--json", "disk.img"])).is_err());
    }

    #[test]
    fn renders_deterministic_text_and_json() {
        let report = sample_report();
        assert_eq!(
            render_text_report(&report),
            "Status:            clean\nFormat:            NullFS 1.2 (valid)\nLabel:             test\"label\nVolume state:      clean\nBlock size:        4096 bytes\nCapacity:          4096 blocks (16777216 bytes)\nData blocks:       total=3900 used=2 free=3898\nInodes:            total=128 used=1 free=127"
        );
        assert_eq!(
            render_json_report(&report),
            "{\"status\":\"clean\",\"valid\":true,\"filesystem\":\"NullFS\",\"format\":{\"major\":1,\"minor\":2},\"label\":\"test\\\"label\",\"volume_state\":\"clean\",\"block_size\":4096,\"capacity\":{\"blocks\":4096,\"bytes\":16777216},\"statistics\":{\"data_blocks\":{\"total\":3900,\"used\":2,\"free\":3898},\"inodes\":{\"total\":128,\"used\":1,\"free\":127}}}"
        );
    }

    #[test]
    fn renders_json_failures_and_escapes_control_characters() {
        let failure = CheckFailure::recovery_required("orphaned \"node\"\nrepair required");
        assert_eq!(
            render_json_failure(&failure),
            "{\"status\":\"recovery-required\",\"valid\":false,\"error\":\"orphaned \\\"node\\\"\\nrepair required\"}"
        );
    }

    #[test]
    fn classifies_persistent_orphans_as_recovery_required() {
        let failure = mount_failure(Path::new("orphan.img"), FilesystemError::RecoveryRequired);
        assert_eq!(failure.kind, FailureKind::RecoveryRequired);
        assert!(failure.message.contains("persistent orphaned inodes"));
        assert!(failure.message.contains("repair is not implemented"));
    }

    #[test]
    fn checks_a_valid_image_without_modifying_it() {
        let image = TempImage::new();
        let device = FileBlockDevice::open(&image.0, BLOCK_SIZE, true).expect("open image");
        let builder = ImageBuilder::new(device, [7; 16], "read-only-check").expect("builder");
        drop(builder.finish().expect("finish image"));
        let before = fs::read(&image.0).expect("read image before check");

        let report = check_image(&image.0).expect("check image");

        let after = fs::read(&image.0).expect("read image after check");
        assert_eq!(before, after);
        assert_eq!(report.label, "read-only-check");
        assert_eq!(report.capacity_blocks, TEST_IMAGE_BLOCKS);
        assert!(report.statistics.is_some());
    }
}
