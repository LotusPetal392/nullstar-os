from pathlib import Path
from textwrap import dedent, indent


def block(text: str, spaces: int) -> str:
    return indent(dedent(text), " " * spaces)


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    if old not in source:
        raise SystemExit(f"{label}: patch target not found")
    return source.replace(old, new, 1)


def replace_between(
    source: str, start_marker: str, end_marker: str, replacement: str, label: str
) -> str:
    if replacement in source:
        return source
    try:
        start = source.index(start_marker)
        end = source.index(end_marker, start)
    except ValueError as error:
        raise SystemExit(f"{label}: function boundary not found") from error
    return source[:start] + replacement + source[end:]


main_path = Path("kernel/src/main.rs")
main = main_path.read_text()
main = replace_once(
    main,
    block(
        '''\
        mod memory;
        mod scheduler;
        mod shell;
        mod storage;
        ''',
        0,
    ),
    block(
        '''\
        mod memory;
        mod process;
        mod scheduler;
        mod shell;
        mod storage;
        mod vfs;
        ''',
        0,
    ),
    "kernel modules",
)
main = replace_once(
    main,
    block(
        '''\
        pub(crate) use memory::allocator;
        pub(crate) use storage::{fat, partition};
        ''',
        0,
    ),
    block(
        '''\
        pub(crate) use memory::allocator;
        pub(crate) use process::elf;
        pub(crate) use storage::{fat, partition};
        ''',
        0,
    ),
    "kernel re-exports",
)

initialization = block(
    '''\
    let vfs_info = if filesystem_info.is_some() {
        match vfs::mount_fat_root() {
            Ok(info) => {
                serial_println!(
                    "VFS initialized: root={}, filesystem={}, read_only={}, label=`{}`, volume_id={:#010x}, partition={}, start_lba={}",
                    info.mount_path,
                    info.filesystem,
                    info.read_only,
                    info.volume_label,
                    info.volume_id,
                    info.partition_index,
                    info.partition_start_lba
                );
                Some(info)
            }
            Err(error) => {
                serial_println!("VFS initialization failed: {error}");
                None
            }
        }
    } else {
        serial_println!("VFS unavailable: no FAT filesystem is mounted");
        None
    };

    let elf_image = if vfs_info.is_some() {
        match elf::validate_first_in_directory("/") {
            Ok(image) => {
                serial_println!(
                    "ELF image validated: path=`{}`, type={}, machine=x86_64, entry={:#018x}, file_bytes={}, program_headers={}, load_segments={}, dynamic={}, tls={}, executable_stack={}",
                    image.path,
                    image.image_type,
                    image.entry_point,
                    image.file_size,
                    image.program_header_count,
                    image.load_segments().len(),
                    image.has_dynamic_segment,
                    image.has_tls_segment,
                    image.executable_stack_requested
                );
                for segment in image.load_segments() {
                    serial_println!(
                        "ELF LOAD: index={}, file={:#x}+{:#x}, virtual={:#018x}+{:#x}, align={:#x}, permissions={}",
                        segment.program_header_index,
                        segment.file_offset,
                        segment.file_size,
                        segment.virtual_address,
                        segment.memory_size,
                        segment.alignment,
                        segment.permissions()
                    );
                }
                Some(image)
            }
            Err(error) => {
                serial_println!("ELF validation failed: {error}");
                None
            }
        }
    } else {
        serial_println!("ELF validation unavailable: VFS is not initialized");
        None
    };

    ''',
    4,
)
if "    let vfs_info = if filesystem_info.is_some() {" not in main:
    marker = "    let scheduler_initial = match scheduler::init() {"
    if marker not in main:
        raise SystemExit("kernel initialization insertion point not found")
    main = main.replace(marker, initialization + marker, 1)

main = replace_once(
    main,
    block(
        '''\
        if filesystem_info.is_some() {
            println!("Read-only FAT filesystem mounted");
        } else {
            println!("FAT filesystem unavailable");
        }
        println!("Interactive shell initialized");
        ''',
        4,
    ),
    block(
        '''\
        if filesystem_info.is_some() {
            println!("Read-only FAT filesystem mounted");
        } else {
            println!("FAT filesystem unavailable");
        }
        if vfs_info.is_some() {
            println!("Virtual filesystem mounted");
        } else {
            println!("Virtual filesystem unavailable");
        }
        if elf_image.is_some() {
            println!("ELF64 image validated");
        } else {
            println!("ELF64 validation unavailable");
        }
        println!("Interactive shell initialized");
        ''',
        4,
    ),
    "boot status",
)
main_path.write_text(main)

shell_path = Path("kernel/src/shell.rs")
shell = shell_path.read_text()
shell = replace_once(
    shell,
    "use crate::{acpi, ahci, allocator, console, fat, interrupts, memory, partition, pci};",
    "use crate::{acpi, ahci, allocator, console, elf, fat, interrupts, memory, partition, pci, vfs};",
    "shell imports",
)
shell = replace_once(
    shell,
    block(
        '''\
        "partitions" => self.print_partitions(),
        "fs" => self.print_filesystem(),
        "ls" => self.list_files(words.next().unwrap_or("/")),
        "cat" => {
            let Some(path) = words.next() else {
                shell_println!("usage: cat <path>");
                return ShellAction::Continue;
            };
            self.cat_file(path);
        }
        ''',
        12,
    ),
    block(
        '''\
        "partitions" => self.print_partitions(),
        "fs" => self.print_filesystem(),
        "vfs" => self.print_vfs(),
        "ls" => self.list_files(words.next().unwrap_or("/")),
        "cat" => {
            let Some(path) = words.next() else {
                shell_println!("usage: cat <path>");
                return ShellAction::Continue;
            };
            self.cat_file(path);
        }
        "elf" => {
            let Some(path) = words.next() else {
                shell_println!("usage: elf <path>");
                return ShellAction::Continue;
            };
            self.inspect_elf(path);
        }
        ''',
        12,
    ),
    "shell command dispatch",
)

list_files = block(
    '''\
    fn list_files(&self, path: &str) {
        let entries = match vfs::read_directory(path) {
            Ok(entries) => entries,
            Err(error) => {
                shell_println!("ls: {error}");
                return;
            }
        };

        shell_println!(
            "{}: {} entr{}",
            path,
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        for entry in entries {
            let kind = if entry.is_directory() { "d" } else { "-" };
            let suffix = if entry.is_directory() { "/" } else { "" };
            shell_println!(
                "{}{}{}{} {:>10} {}{}",
                kind,
                if entry.read_only { "r" } else { "-" },
                if entry.hidden { "h" } else { "-" },
                if entry.system { "s" } else { "-" },
                entry.size,
                entry.name,
                suffix
            );
        }
    }

    ''',
    4,
)
shell = replace_between(
    shell,
    "    fn list_files(&self, path: &str) {",
    "    fn cat_file(&self, path: &str) {",
    list_files,
    "VFS directory listing",
)

cat_and_diagnostics = block(
    '''\
    fn cat_file(&self, path: &str) {
        let data = match vfs::read_file(path, FILE_PREVIEW_BYTES) {
            Ok(data) => data,
            Err(error) => {
                shell_println!("cat: {error}");
                return;
            }
        };

        shell_println!(
            "{}: {} bytes{}",
            path,
            data.total_size,
            if data.truncated {
                " (preview truncated)"
            } else {
                ""
            }
        );
        let mut ended_with_newline = true;
        for byte in data.bytes {
            match byte {
                b'\n' => {
                    shell_println!();
                    ended_with_newline = true;
                }
                b'\r' => {}
                b'\t' => {
                    shell_print!("    ");
                    ended_with_newline = false;
                }
                0x20..=0x7e => {
                    shell_print!("{}", char::from(byte));
                    ended_with_newline = false;
                }
                _ => {
                    shell_print!(".");
                    ended_with_newline = false;
                }
            }
        }
        if !ended_with_newline {
            shell_println!();
        }
        if data.truncated {
            shell_println!("cat: output limited to {} bytes", FILE_PREVIEW_BYTES);
        }
    }

    fn print_vfs(&self) {
        let Some(info) = vfs::info() else {
            shell_println!("VFS: unavailable");
            return;
        };

        shell_println!(
            "VFS: {} mounted at {}, read-only={}",
            info.filesystem,
            info.mount_path,
            info.read_only
        );
        shell_println!(
            "backend: volume=`{}`, id={:#010x}, partition={}, start_lba={}",
            info.volume_label,
            info.volume_id,
            info.partition_index,
            info.partition_start_lba
        );
        shell_println!(
            "limits: path_bytes={}, path_components={}, read_window={} KiB",
            vfs::MAX_PATH_BYTES,
            vfs::MAX_PATH_COMPONENTS,
            vfs::MAX_READ_WINDOW_BYTES / 1024
        );
    }

    fn inspect_elf(&self, path: &str) {
        let image = match elf::validate(path) {
            Ok(image) => image,
            Err(error) => {
                shell_println!("elf: {error}");
                return;
            }
        };

        shell_println!("ELF64 x86-64 {}: `{}`", image.image_type, image.path);
        shell_println!(
            "entry={:#018x}, file={} bytes, program headers={}, LOAD segments={}",
            image.entry_point,
            image.file_size,
            image.program_header_count,
            image.load_segments().len()
        );
        shell_println!(
            "dynamic={}, TLS={}, executable stack requested={}",
            image.has_dynamic_segment,
            image.has_tls_segment,
            image.executable_stack_requested
        );
        for segment in image.load_segments() {
            shell_println!(
                "LOAD[{}] {} file={:#x}+{:#x} virtual={:#018x}+{:#x} align={:#x}",
                segment.program_header_index,
                segment.permissions(),
                segment.file_offset,
                segment.file_size,
                segment.virtual_address,
                segment.memory_size,
                segment.alignment
            );
        }
    }

    ''',
    4,
)
shell = replace_between(
    shell,
    "    fn cat_file(&self, path: &str) {",
    "    fn print_interrupts(&self) {",
    cat_and_diagnostics,
    "VFS file and ELF diagnostics",
)

shell = replace_once(
    shell,
    block(
        '''\
        shell_println!("  fs               show the mounted FAT volume");
        shell_println!("  ls [path]        list a FAT directory");
        shell_println!("  cat <path>       preview a FAT file");
        ''',
        4,
    ),
    block(
        '''\
        shell_println!("  fs               show the mounted FAT volume");
        shell_println!("  vfs              show the root VFS mount");
        shell_println!("  ls [path]        list a VFS directory");
        shell_println!("  cat <path>       preview a VFS file");
        shell_println!("  elf <path>       validate an ELF64 executable");
        ''',
        4,
    ),
    "shell help",
)
shell_path.write_text(shell)

launcher_path = Path("src/main.rs")
launcher = launcher_path.read_text()
launcher = replace_once(
    launcher,
    block(
        '''\
        const PARTITION_TEST_MARKER: &str = "partition table initialized:";
        const FAT_TEST_MARKER: &str = "FAT filesystem mounted:";
        ''',
        0,
    ),
    block(
        '''\
        const PARTITION_TEST_MARKER: &str = "partition table initialized:";
        const FAT_TEST_MARKER: &str = "FAT filesystem mounted:";
        const VFS_TEST_MARKER: &str = "VFS initialized:";
        const ELF_TEST_MARKER: &str = "ELF image validated:";
        ''',
        0,
    ),
    "launcher markers",
)
launcher = replace_once(
    launcher,
    block(
        '''\
        let mut partitions_ready = false;
        let mut fat_ready = false;
        ''',
        8,
    ),
    block(
        '''\
        let mut partitions_ready = false;
        let mut fat_ready = false;
        let mut vfs_ready = false;
        let mut elf_ready = false;
        ''',
        8,
    ),
    "launcher state",
)
launcher = replace_once(
    launcher,
    block(
        '''\
        partitions_ready |= line.contains(PARTITION_TEST_MARKER);
        fat_ready |= line.contains(FAT_TEST_MARKER);
        ''',
        12,
    ),
    block(
        '''\
        partitions_ready |= line.contains(PARTITION_TEST_MARKER);
        fat_ready |= line.contains(FAT_TEST_MARKER);
        vfs_ready |= line.contains(VFS_TEST_MARKER);
        elf_ready |= line.contains(ELF_TEST_MARKER);
        ''',
        12,
    ),
    "launcher marker collection",
)
launcher = replace_once(
    launcher,
    block(
        '''\
        && partitions_ready
        && fat_ready
        ''',
        16,
    ),
    block(
        '''\
        && partitions_ready
        && fat_ready
        && vfs_ready
        && elf_ready
        ''',
        16,
    ),
    "launcher completion condition",
)
launcher = replace_once(
    launcher,
    block(
        '''\
        println!(
            "  --test      Verify heap, framebuffer, ACPI, timers, scheduler, PCIe, AHCI, partitions, and FAT"
        );
        ''',
        4,
    ),
    block(
        '''\
        println!(
            "  --test      Verify heap, framebuffer, ACPI, timers, scheduler, storage, VFS, and ELF"
        );
        ''',
        4,
    ),
    "launcher usage",
)
launcher_path.write_text(launcher)
