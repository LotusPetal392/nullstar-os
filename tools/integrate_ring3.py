from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    if old not in source:
        raise SystemExit(f"{label}: patch target not found")
    return source.replace(old, new, 1)


main_path = Path("kernel/src/main.rs")
main = main_path.read_text()
main = replace_once(
    main,
    "pub(crate) use process::elf;",
    "pub(crate) use process::{elf, userspace};",
    "userspace re-export",
)

if "userspace process exited:" not in main:
    marker = '    println!("GalacticOS");\n'
    if marker not in main:
        raise SystemExit("userspace launch insertion point not found")
    block = r'''    let userspace_result = match elf::validate("/init") {
        Ok(init_image) => match userspace::run(
            "/init",
            &init_image,
            &mut mapper,
            &mut frame_allocator,
            physical_memory_offset,
        ) {
            Ok(result) => {
                serial_println!(
                    "userspace process exited: path={}, exit_code={}, entry={:#018x}, page_table={:#x}, mapped_pages={}, load_segments={}, user_stack_bytes={}, guard_page={:#018x}, kernel_stack_bytes={}, syscalls={}, writes={}, yields={}, bytes_written={}",
                    result.path,
                    result.exit_code,
                    result.entry_point,
                    result.page_table_address,
                    result.mapped_pages,
                    result.load_segments,
                    result.user_stack_bytes,
                    result.guard_page_address,
                    result.kernel_stack_bytes,
                    result.syscall_count,
                    result.write_count,
                    result.yield_count,
                    result.bytes_written
                );
                Some(result)
            }
            Err(error) => {
                serial_println!("userspace process failed: {error}");
                None
            }
        },
        Err(error) => {
            serial_println!("userspace init validation failed: {error}");
            None
        }
    };

'''
    main = main.replace(marker, block + marker, 1)

banner_old = '''    if elf_image.is_some() {
        println!("ELF64 image validated");
    } else {
        println!("ELF64 validation unavailable");
    }
    println!("Interactive shell initialized");
'''
banner_new = '''    if elf_image.is_some() {
        println!("ELF64 image validated");
    } else {
        println!("ELF64 validation unavailable");
    }
    if userspace_result.is_some() {
        println!("First ring-3 process exited");
    } else {
        println!("Ring-3 process unavailable");
    }
    println!("Interactive shell initialized");
'''
main = replace_once(main, banner_old, banner_new, "userspace banner")
main_path.write_text(main)

shell_path = Path("kernel/src/shell.rs")
shell = shell_path.read_text()
shell = replace_once(
    shell,
    "use crate::{acpi, ahci, allocator, console, elf, fat, interrupts, memory, partition, pci, vfs};",
    "use crate::{\n    acpi, ahci, allocator, console, elf, fat, interrupts, memory, partition, pci, userspace,\n    vfs,\n};",
    "userspace shell import",
)

command_old = '''            "elf" => {
                let Some(path) = words.next() else {
                    shell_println!("usage: elf <path>");
                    return ShellAction::Continue;
                };
                self.inspect_elf(path);
            }
            "about" => {
'''
command_new = '''            "elf" => {
                let Some(path) = words.next() else {
                    shell_println!("usage: elf <path>");
                    return ShellAction::Continue;
                };
                self.inspect_elf(path);
            }
            "process" | "userspace" => self.print_userspace(),
            "about" => {
'''
shell = replace_once(shell, command_old, command_new, "userspace shell command")

if "fn print_userspace(&self)" not in shell:
    marker = "    fn print_interrupts(&self) {\n"
    if marker not in shell:
        raise SystemExit("userspace diagnostics insertion point not found")
    method = r'''    fn print_userspace(&self) {
        let Some(result) = userspace::last_result() else {
            shell_println!("userspace: no process has completed");
            return;
        };

        shell_println!(
            "userspace: `{}` exited with code {}",
            result.path,
            result.exit_code
        );
        shell_println!(
            "entry={:#018x}, page table={:#x}, mapped pages={}, LOAD segments={}",
            result.entry_point,
            result.page_table_address,
            result.mapped_pages,
            result.load_segments
        );
        shell_println!(
            "stacks: user={} KiB, guard={:#018x}, kernel={} KiB",
            result.user_stack_bytes / 1024,
            result.guard_page_address,
            result.kernel_stack_bytes / 1024
        );
        shell_println!(
            "syscalls: total={}, writes={}, yields={}, bytes written={}",
            result.syscall_count,
            result.write_count,
            result.yield_count,
            result.bytes_written
        );
    }

'''
    shell = shell.replace(marker, method + marker, 1)

help_old = '    shell_println!("  elf <path>       validate an ELF64 executable");\n'
help_new = help_old + '    shell_println!("  process          show the completed ring-3 process");\n'
shell = replace_once(shell, help_old, help_new, "userspace help entry")
shell_path.write_text(shell)

runner_path = Path("src/main.rs")
runner = runner_path.read_text()
runner = replace_once(
    runner,
    'const ELF_TEST_MARKER: &str = "ELF image validated:";\n',
    'const ELF_TEST_MARKER: &str = "ELF image validated:";\nconst USERSPACE_TEST_MARKER: &str =\n    "userspace process exited: path=/init, exit_code=42";\n',
    "userspace smoke marker",
)
runner = replace_once(
    runner,
    "const QEMU_TEST_TIMEOUT: Duration = Duration::from_secs(45);",
    "const QEMU_TEST_TIMEOUT: Duration = Duration::from_secs(60);",
    "userspace smoke timeout",
)
runner = replace_once(
    runner,
    '        "  --test      Verify heap, framebuffer, ACPI, timers, scheduler, storage, VFS, and ELF"',
    '        "  --test      Verify hardware, storage, VFS, ELF, and the first ring-3 process"',
    "userspace usage text",
)
runner = replace_once(
    runner,
    "        let mut elf_ready = false;\n",
    "        let mut elf_ready = false;\n        let mut userspace_ready = false;\n",
    "userspace readiness state",
)
runner = replace_once(
    runner,
    "            elf_ready |= line.contains(ELF_TEST_MARKER);\n",
    "            elf_ready |= line.contains(ELF_TEST_MARKER);\n            userspace_ready |= line.contains(USERSPACE_TEST_MARKER);\n",
    "userspace marker observation",
)
runner = replace_once(
    runner,
    "                && elf_ready\n",
    "                && elf_ready\n                && userspace_ready\n",
    "userspace smoke condition",
)
runner_path.write_text(runner)
