use std::{
    env,
    io::{self, BufRead, BufReader, Write},
    process::{Command, ExitCode, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const HEAP_TEST_MARKER: &str = "heap allocation self-test passed:";
const FRAMEBUFFER_TEST_MARKER: &str = "framebuffer shadow buffer initialized:";
const ACPI_TEST_MARKER: &str = "ACPI initialized:";
const LAPIC_TIMER_TEST_MARKER: &str = "interrupt timer verified: controller=apic, source=lapic";
const SCHEDULER_TEST_MARKER: &str = "scheduler verified:";
const PCIE_TEST_MARKER: &str = "PCIe initialized:";
const AHCI_TEST_MARKER: &str = "AHCI storage verified:";
const PARTITION_TEST_MARKER: &str = "partition table initialized:";
const FAT_TEST_MARKER: &str = "FAT filesystem mounted:";
const VFS_TEST_MARKER: &str = "VFS initialized:";
const ELF_TEST_MARKER: &str = "ELF image validated:";
const USERSPACE_TEST_MARKER: &str = "process isolation verified:";
const USER_FILE_IO_TEST_MARKER: &str = "userspace file I/O verified:";
const USER_TERMINAL_TEST_MARKER: &str = "userspace terminal verified:";
const USER_PIPE_TEST_MARKER: &str = "userspace pipe verified:";
const USER_SHELL_TEST_MARKER: &str = "userspace shell verified:";
const USER_PIPELINE_OUTPUT_MARKER: &str = "HELLO THROUGH A BLOCKING GALACTICOS PIPE.";
const USER_PIPELINE_TEST_MARKER: &str = "userspace multi-stage pipeline verified:";
const USER_BACKGROUND_TEST_MARKER: &str = "userspace background jobs verified:";
const QEMU_TEST_TIMEOUT: Duration = Duration::from_secs(75);

#[derive(Debug, Default)]
struct Options {
    headless: bool,
    test: bool,
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(exit_code) => return exit_code,
    };

    let command = qemu_command(&options);

    if options.test {
        run_kernel_smoke_test(command)
    } else {
        run_interactive(command)
    }
}

fn parse_options() -> Result<Options, ExitCode> {
    let mut options = Options::default();

    for argument in env::args().skip(1) {
        match argument.as_str() {
            "--headless" => options.headless = true,
            "--test" => {
                options.test = true;
                options.headless = true;
            }
            "-h" | "--help" => {
                print_usage();
                return Err(ExitCode::SUCCESS);
            }
            _ => {
                eprintln!("Unknown argument: {argument}");
                print_usage();
                return Err(ExitCode::from(2));
            }
        }
    }

    Ok(options)
}

fn print_usage() {
    println!("Usage: cargo run -- [--headless] [--test]");
    println!("  --headless  Disable the QEMU display and use serial output only");
    println!(
        "  --test      Verify hardware, storage, VFS, process control, terminal input, and multi-stage userspace pipelines"
    );
}

fn qemu_command(options: &Options) -> Command {
    let bios_image = env!("BIOS_IMAGE");
    let mut command = Command::new("qemu-system-x86_64");

    command
        .args(["-machine", "q35"])
        .arg("-drive")
        .arg(format!("if=none,id=bootdisk,format=raw,file={bios_image}"))
        .args(["-device", "ide-hd,drive=bootdisk,bus=ide.0,bootindex=1"])
        .args(["-serial", "stdio", "-monitor", "none", "-m", "128M"]);

    if options.headless {
        command.args(["-display", "none"]);
    }

    if options.test {
        command.args(["-no-reboot", "-no-shutdown"]);
    }

    command
}

fn run_interactive(mut command: Command) -> ExitCode {
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("QEMU exited with status: {status}");
            ExitCode::FAILURE
        }
        Err(error) => qemu_start_error(error),
    }
}

fn run_kernel_smoke_test(mut command: Command) -> ExitCode {
    command.stdout(Stdio::piped()).stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return qemu_start_error(error),
    };

    let Some(serial_output) = child.stdout.take() else {
        eprintln!("QEMU serial output was not captured");
        let _ = child.kill();
        let _ = child.wait();
        return ExitCode::FAILURE;
    };

    let (marker_sender, marker_receiver) = mpsc::channel();
    let reader = thread::spawn(move || -> io::Result<()> {
        let mut terminal = io::stdout().lock();
        let mut heap_ready = false;
        let mut framebuffer_ready = false;
        let mut acpi_ready = false;
        let mut lapic_timer_ready = false;
        let mut scheduler_ready = false;
        let mut pcie_ready = false;
        let mut ahci_ready = false;
        let mut partitions_ready = false;
        let mut fat_ready = false;
        let mut vfs_ready = false;
        let mut elf_ready = false;
        let mut userspace_ready = false;
        let mut user_file_io_ready = false;
        let mut user_terminal_ready = false;
        let mut user_pipe_ready = false;
        let mut user_shell_ready = false;
        let mut user_pipeline_output_ready = false;
        let mut user_pipeline_ready = false;
        let mut user_background_ready = false;

        for line in BufReader::new(serial_output).lines() {
            let line = line?;
            writeln!(terminal, "{line}")?;
            terminal.flush()?;

            heap_ready |= line.contains(HEAP_TEST_MARKER);
            framebuffer_ready |= line.contains(FRAMEBUFFER_TEST_MARKER);
            acpi_ready |= line.contains(ACPI_TEST_MARKER);
            lapic_timer_ready |= line.contains(LAPIC_TIMER_TEST_MARKER);
            scheduler_ready |= line.contains(SCHEDULER_TEST_MARKER);
            pcie_ready |= line.contains(PCIE_TEST_MARKER);
            ahci_ready |= line.contains(AHCI_TEST_MARKER);
            partitions_ready |= line.contains(PARTITION_TEST_MARKER);
            fat_ready |= line.contains(FAT_TEST_MARKER);
            vfs_ready |= line.contains(VFS_TEST_MARKER);
            elf_ready |= line.contains(ELF_TEST_MARKER);
            userspace_ready |= line.contains(USERSPACE_TEST_MARKER);
            user_file_io_ready |= line.contains(USER_FILE_IO_TEST_MARKER);
            user_terminal_ready |= line.contains(USER_TERMINAL_TEST_MARKER);
            user_pipe_ready |= line.contains(USER_PIPE_TEST_MARKER);
            user_shell_ready |= line.contains(USER_SHELL_TEST_MARKER);
            user_pipeline_output_ready |= line.contains(USER_PIPELINE_OUTPUT_MARKER);
            user_pipeline_ready |= line.contains(USER_PIPELINE_TEST_MARKER);
            user_background_ready |= line.contains(USER_BACKGROUND_TEST_MARKER);

            if heap_ready
                && framebuffer_ready
                && acpi_ready
                && lapic_timer_ready
                && scheduler_ready
                && pcie_ready
                && ahci_ready
                && partitions_ready
                && fat_ready
                && vfs_ready
                && elf_ready
                && userspace_ready
                && user_file_io_ready
                && user_terminal_ready
                && user_pipe_ready
                && user_shell_ready
                && user_pipeline_output_ready
                && user_pipeline_ready
                && user_background_ready
            {
                let _ = marker_sender.send(());
                break;
            }
        }

        Ok(())
    });

    let deadline = Instant::now() + QEMU_TEST_TIMEOUT;

    loop {
        match marker_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                println!("QEMU kernel smoke test passed");
                return ExitCode::SUCCESS;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                report_reader_result(reader.join());
                eprintln!("QEMU stopped producing serial output before the kernel test passed");
                return ExitCode::FAILURE;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                report_reader_result(reader.join());
                eprintln!("QEMU exited with status {status} before the kernel test passed");
                return ExitCode::FAILURE;
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                report_reader_result(reader.join());
                eprintln!("Could not query QEMU status: {error}");
                return ExitCode::FAILURE;
            }
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            report_reader_result(reader.join());
            eprintln!(
                "QEMU kernel smoke test timed out after {} seconds",
                QEMU_TEST_TIMEOUT.as_secs()
            );
            return ExitCode::FAILURE;
        }
    }
}

fn report_reader_result(result: thread::Result<io::Result<()>>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("Failed to read QEMU serial output: {error}"),
        Err(_) => eprintln!("QEMU serial reader thread panicked"),
    }
}

fn qemu_start_error(error: io::Error) -> ExitCode {
    eprintln!("Could not start QEMU: {error}");
    eprintln!("Make sure qemu-system-x86_64 is installed.");
    ExitCode::FAILURE
}
