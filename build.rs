use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use nullfs_blockdev::MemoryBlockDevice;
use nullfs_format::BLOCK_SIZE;
use nullfs_testkit::ImageBuilder;

#[allow(dead_code)]
mod nullfs_primary_volume {
    include!("shared/nullfs_primary_volume.rs");
}

const HELLO_TEXT: &str = "Hello from a NullStar OS userspace file descriptor.\n";
const NORMAL_BOOT_MODE: &[u8] = b"normal\n";
const SMOKE_TEST_BOOT_MODE: &[u8] = b"smoke-test\n";
const NULLFS_RESTART_TEST_BOOT_MODE: &[u8] = b"nullfs-restart-test\n";

const NULLFS_MBR_TYPE: u8 = 0x7f;
const MBR_BYTES: usize = 512;
const MBR_PARTITION_TABLE_OFFSET: usize = 446;
const MBR_PARTITION_ENTRY_BYTES: usize = 16;
const NULLFS_MBR_SLOT: usize = 2;

fn main() {
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR was not set"));

    let kernel_binary = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_KERNEL_kernel").expect("kernel artifact path was not set"),
    );
    let userspace_init = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_init")
            .expect("userspace init artifact path was not set"),
    );
    let userspace_nullfs_service = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_NULLFS_SERVICE_nullfs_service")
            .expect("userspace NullFS service artifact path was not set"),
    );
    let userspace_block_device_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_block_device_probe")
            .expect("userspace block-device-probe artifact path was not set"),
    );
    let userspace_nullfs_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_nullfs_probe")
            .expect("userspace NullFS probe artifact path was not set"),
    );
    let userspace_tmpfs_service = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_tmpfs_service")
            .expect("userspace tmpfs-service artifact path was not set"),
    );
    let userspace_tmpfs_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_tmpfs_probe")
            .expect("userspace tmpfs-probe artifact path was not set"),
    );
    let userspace_vfs_service = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_vfs_service")
            .expect("userspace vfs-service artifact path was not set"),
    );
    let userspace_vfs_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_vfs_probe")
            .expect("userspace vfs-probe artifact path was not set"),
    );
    let userspace_logging_service = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_logging_service")
            .expect("userspace logging-service artifact path was not set"),
    );
    let userspace_logging_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_logging_probe")
            .expect("userspace logging-probe artifact path was not set"),
    );
    let userspace_service_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_service_probe")
            .expect("userspace service-probe artifact path was not set"),
    );
    let userspace_process_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_process_probe")
            .expect("userspace process-probe artifact path was not set"),
    );
    let userspace_fault_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_fault_probe")
            .expect("userspace fault-probe artifact path was not set"),
    );
    let userspace_cat = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_cat")
            .expect("userspace cat artifact path was not set"),
    );
    let userspace_ls = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_ls").expect("userspace ls artifact path was not set"),
    );
    let userspace_pwd = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_pwd")
            .expect("userspace pwd artifact path was not set"),
    );
    let userspace_stat = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_stat")
            .expect("userspace stat artifact path was not set"),
    );
    let userspace_readline = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_readline")
            .expect("userspace readline artifact path was not set"),
    );
    let userspace_pipe_producer = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_pipe_producer")
            .expect("userspace pipe-producer artifact path was not set"),
    );
    let userspace_pipe_consumer = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_pipe_consumer")
            .expect("userspace pipe-consumer artifact path was not set"),
    );
    let userspace_upper = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_upper")
            .expect("userspace upper artifact path was not set"),
    );
    let userspace_delay = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_delay")
            .expect("userspace delay artifact path was not set"),
    );
    let userspace_signal_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_signal_probe")
            .expect("userspace signal-probe artifact path was not set"),
    );
    let userspace_runtime_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_runtime_probe")
            .expect("userspace runtime-probe artifact path was not set"),
    );
    let userspace_stderr_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_stderr_probe")
            .expect("userspace stderr-probe artifact path was not set"),
    );
    let userspace_exec = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_exec")
            .expect("userspace exec launcher artifact path was not set"),
    );
    let userspace_exec_source = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_exec_source")
            .expect("userspace exec-source artifact path was not set"),
    );
    let userspace_exec_target = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_exec_target")
            .expect("userspace exec-target artifact path was not set"),
    );
    let userspace_fork_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_fork_probe")
            .expect("userspace fork-probe artifact path was not set"),
    );
    let userspace_fork_target = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_fork_target")
            .expect("userspace fork-target artifact path was not set"),
    );
    let userspace_signal_handler_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_signal_handler_probe")
            .expect("userspace signal-handler-probe artifact path was not set"),
    );
    let userspace_signal_lifecycle_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_signal_lifecycle_probe")
            .expect("userspace signal-lifecycle-probe artifact path was not set"),
    );
    let userspace_signal_lifecycle_target = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_signal_lifecycle_target")
            .expect("userspace signal-lifecycle-target artifact path was not set"),
    );
    let userspace_environment_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_environment_probe")
            .expect("userspace environment-probe artifact path was not set"),
    );
    let userspace_environment_target = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_environment_target")
            .expect("userspace environment-target artifact path was not set"),
    );
    let userspace_shell = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_ush")
            .expect("userspace shell artifact path was not set"),
    );
    let userspace_logctl = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_logctl")
            .expect("userspace logctl artifact path was not set"),
    );
    let userspace_sv = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_sv").expect("userspace sv artifact path was not set"),
    );
    let hello_text = output_directory.join("hello.txt");
    fs::write(&hello_text, HELLO_TEXT).expect("failed to create userspace file-I/O fixture");

    let build_image = |boot_mode: &[u8]| {
        let mut image = bootloader::DiskImageBuilder::new(kernel_binary.clone());
        image.set_file_contents(String::from("BOOTMODE"), boot_mode.to_vec());
        image.set_file(String::from("init"), userspace_init.clone());
        image.set_file(
            String::from("nullfs-service"),
            userspace_nullfs_service.clone(),
        );
        image.set_file(
            String::from("block-device-probe"),
            userspace_block_device_probe.clone(),
        );
        image.set_file(String::from("nullfs-probe"), userspace_nullfs_probe.clone());
        image.set_file(
            String::from("tmpfs-service"),
            userspace_tmpfs_service.clone(),
        );
        image.set_file(String::from("tmpfs-probe"), userspace_tmpfs_probe.clone());
        image.set_file(String::from("vfs-service"), userspace_vfs_service.clone());
        image.set_file(String::from("vfs-probe"), userspace_vfs_probe.clone());
        image.set_file(
            String::from("logging-service"),
            userspace_logging_service.clone(),
        );
        image.set_file(
            String::from("logging-probe"),
            userspace_logging_probe.clone(),
        );
        image.set_file(
            String::from("service-probe"),
            userspace_service_probe.clone(),
        );
        image.set_file(
            String::from("process-probe"),
            userspace_process_probe.clone(),
        );
        image.set_file(String::from("fault-probe"), userspace_fault_probe.clone());
        image.set_file(String::from("cat"), userspace_cat.clone());
        image.set_file(String::from("ls"), userspace_ls.clone());
        image.set_file(String::from("pwd"), userspace_pwd.clone());
        image.set_file(String::from("stat"), userspace_stat.clone());
        image.set_file(String::from("readline"), userspace_readline.clone());
        image.set_file(
            String::from("pipe-producer"),
            userspace_pipe_producer.clone(),
        );
        image.set_file(
            String::from("pipe-consumer"),
            userspace_pipe_consumer.clone(),
        );
        image.set_file(String::from("upper"), userspace_upper.clone());
        image.set_file(String::from("delay"), userspace_delay.clone());
        image.set_file(String::from("signal-probe"), userspace_signal_probe.clone());
        image.set_file(
            String::from("runtime-probe"),
            userspace_runtime_probe.clone(),
        );
        image.set_file(String::from("stderr-probe"), userspace_stderr_probe.clone());
        image.set_file(String::from("exec"), userspace_exec.clone());
        image.set_file(String::from("exec-source"), userspace_exec_source.clone());
        image.set_file(String::from("exec-target"), userspace_exec_target.clone());
        image.set_file(String::from("fork-probe"), userspace_fork_probe.clone());
        image.set_file(String::from("fork-target"), userspace_fork_target.clone());
        image.set_file(
            String::from("signal-handler-probe"),
            userspace_signal_handler_probe.clone(),
        );
        image.set_file(
            String::from("signal-lifecycle-probe"),
            userspace_signal_lifecycle_probe.clone(),
        );
        image.set_file(
            String::from("signal-lifecycle-target"),
            userspace_signal_lifecycle_target.clone(),
        );
        image.set_file(
            String::from("environment-probe"),
            userspace_environment_probe.clone(),
        );
        image.set_file(
            String::from("environment-target"),
            userspace_environment_target.clone(),
        );
        image.set_file(String::from("ush"), userspace_shell.clone());
        image.set_file(String::from("logctl"), userspace_logctl.clone());
        image.set_file(String::from("sv"), userspace_sv.clone());
        image.set_file(String::from("hello.txt"), hello_text.clone());
        image
    };

    let nullfs_fixture = build_nullfs_fixture();
    let bios_image = output_directory.join("nullstar-os-bios.img");
    build_image(NORMAL_BOOT_MODE)
        .create_bios_image(&bios_image)
        .expect("failed to create BIOS disk image");
    append_nullfs_partition(&bios_image, &nullfs_fixture)
        .expect("failed to append NullFS partition to BIOS disk image");
    let smoke_test_bios_image = output_directory.join("nullstar-os-smoke-test-bios.img");
    build_image(SMOKE_TEST_BOOT_MODE)
        .create_bios_image(&smoke_test_bios_image)
        .expect("failed to create smoke-test BIOS disk image");
    append_nullfs_partition(&smoke_test_bios_image, &nullfs_fixture)
        .expect("failed to append NullFS partition to smoke-test BIOS disk image");
    let nullfs_restart_test_bios_image =
        output_directory.join("nullstar-os-nullfs-restart-test-bios.img");
    build_image(NULLFS_RESTART_TEST_BOOT_MODE)
        .create_bios_image(&nullfs_restart_test_bios_image)
        .expect("failed to create NullFS restart-test BIOS disk image");
    append_nullfs_partition(&nullfs_restart_test_bios_image, &nullfs_fixture)
        .expect("failed to append NullFS partition to restart-test BIOS disk image");

    println!("cargo:rustc-env=BIOS_IMAGE={}", bios_image.display());
    println!(
        "cargo:rustc-env=SMOKE_TEST_BIOS_IMAGE={}",
        smoke_test_bios_image.display()
    );
    println!(
        "cargo:rustc-env=NULLFS_RESTART_TEST_BIOS_IMAGE={}",
        nullfs_restart_test_bios_image.display()
    );
}

fn build_nullfs_fixture() -> Vec<u8> {
    let device = MemoryBlockDevice::new(BLOCK_SIZE, nullfs_primary_volume::CAPACITY_BLOCKS)
        .expect("failed to allocate NullFS fixture device");
    let mut image = ImageBuilder::new(
        device,
        nullfs_primary_volume::FILESYSTEM_UUID,
        nullfs_primary_volume::DISPLAY_NAME,
    )
    .expect("failed to format NullFS fixture");
    for directory in ["System", "Applications", "Users"] {
        image
            .create_directory(1, directory, 0o755)
            .unwrap_or_else(|_| panic!("failed to create NullFS {directory} directory"));
    }
    let docs = image
        .create_directory(1, "docs", 0o755)
        .expect("failed to create NullFS fixture directory");
    image
        .create_file(
            1,
            "welcome.txt",
            b"NullStar persistent storage service fixture.\n",
            0o644,
        )
        .expect("failed to create NullFS fixture root file");
    image
        .create_file(
            docs,
            "readme.txt",
            b"This volume is a deterministic NullFS integration fixture.\n",
            0o644,
        )
        .expect("failed to create NullFS fixture nested file");
    image
        .create_file(
            1,
            "unmanaged.txt",
            b"This entry verifies that boot probes preserve non-reserved data.\n",
            0o644,
        )
        .expect("failed to create unmanaged NullFS fixture file");
    image
        .create_file(
            docs,
            "unmanaged-note.txt",
            b"Nested non-reserved data must not invalidate boot probes.\n",
            0o644,
        )
        .expect("failed to create nested unmanaged NullFS fixture file");
    image
        .finish()
        .expect("failed to finalize NullFS fixture")
        .bytes()
        .to_vec()
}

fn append_nullfs_partition(image_path: &Path, fixture: &[u8]) -> io::Result<()> {
    if fixture.is_empty() || !fixture.len().is_multiple_of(MBR_BYTES) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NullFS fixture must contain complete disk sectors",
        ));
    }

    let mut image = OpenOptions::new().read(true).write(true).open(image_path)?;
    let original_length = image.metadata()?.len();
    let alignment = BLOCK_SIZE as u64;
    let partition_offset = original_length
        .checked_add(alignment - 1)
        .map(|length| length / alignment * alignment)
        .ok_or_else(|| io::Error::other("NullFS partition alignment overflowed"))?;
    let start_lba = u32::try_from(partition_offset / MBR_BYTES as u64)
        .map_err(|_| io::Error::other("NullFS partition start exceeds MBR limits"))?;
    let sector_count = u32::try_from(fixture.len() / MBR_BYTES)
        .map_err(|_| io::Error::other("NullFS partition size exceeds MBR limits"))?;

    let mut mbr = [0_u8; MBR_BYTES];
    image.seek(SeekFrom::Start(0))?;
    image.read_exact(&mut mbr)?;
    if mbr[510..512] != [0x55, 0xaa] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot image is missing the MBR signature",
        ));
    }
    let entry_offset = MBR_PARTITION_TABLE_OFFSET + NULLFS_MBR_SLOT * MBR_PARTITION_ENTRY_BYTES;
    let entry = &mut mbr[entry_offset..entry_offset + MBR_PARTITION_ENTRY_BYTES];
    if entry.iter().any(|byte| *byte != 0) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "boot image already uses MBR partition slot 3",
        ));
    }
    entry[4] = NULLFS_MBR_TYPE;
    entry[8..12].copy_from_slice(&start_lba.to_le_bytes());
    entry[12..16].copy_from_slice(&sector_count.to_le_bytes());

    image.seek(SeekFrom::Start(original_length))?;
    let padding = usize::try_from(partition_offset - original_length)
        .map_err(|_| io::Error::other("NullFS partition padding exceeds host limits"))?;
    image.write_all(&[0_u8; BLOCK_SIZE][..padding])?;
    image.write_all(fixture)?;
    image.seek(SeekFrom::Start(0))?;
    image.write_all(&mbr)?;
    image.sync_all()
}
