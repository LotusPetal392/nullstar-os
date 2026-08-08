use std::{
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
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
const FAT_PERSIST_PREPARED_MARKER: &str = "persistent FAT write prepared:";
const FAT_PERSIST_VERIFIED_MARKER: &str = "persistent FAT write verified:";
const VFS_TEST_MARKER: &str = "VFS initialized:";
const ELF_TEST_MARKER: &str = "ELF image validated:";
const USERSPACE_TEST_MARKER: &str = "process isolation verified:";
const USER_FILE_IO_TEST_MARKER: &str = "userspace file I/O verified:";
const USER_RUST_RUNTIME_TEST_MARKER: &str = "userspace Rust runtime verified:";
const USER_EXEC_TEST_MARKER: &str = "userspace transactional exec verified:";
const USER_FORK_TEST_MARKER: &str = "userspace copy-on-write fork verified:";
const USER_ENVIRONMENT_TEST_MARKER: &str = "userspace environments verified:";
const USER_TERMINAL_TEST_MARKER: &str = "userspace terminal verified:";
const USER_PIPE_TEST_MARKER: &str = "userspace pipe verified:";
const USER_SHELL_TEST_MARKER: &str = "userspace shell verified:";
const USER_PIPELINE_OUTPUT_MARKER: &str = "HELLO THROUGH A BLOCKING NULLSTAR OS PIPE.";
const USER_PIPELINE_TEST_MARKER: &str = "userspace multi-stage pipeline verified:";
const USER_BACKGROUND_TEST_MARKER: &str = "userspace background jobs verified:";
const USER_STOPPED_JOB_TEST_MARKER: &str = "userspace stopped jobs verified:";
const USER_TMPFS_TEST_MARKER: &str = "userspace tmpfs redirection verified:";
const USER_SIGNAL_TEST_MARKER: &str = "userspace process groups and signals verified:";
const USER_SIGNAL_HANDLER_TEST_MARKER: &str = "userspace handled signals verified:";
const NORMAL_BOOT_MODE_MARKER: &str = "boot mode selected: normal";
const NORMAL_BOOT_EARLY_LOG_MARKER: &str = "kernel early log ready: capacity=64, retained=3, overwritten=0, dropped=0, rejected=0, busy_drops=0";
const NORMAL_BOOT_READY_MARKER: &str = "normal boot ready:";
const NORMAL_BOOT_INIT_MARKER: &str = "userspace init ready: pid=1";
const NORMAL_BOOT_LOGGING_IMPORT_MARKER: &str = "logging-service: kernel early log imported";
const NORMAL_BOOT_LOGGING_SERVICE_MARKER: &str = "userspace init: logging service ready";
const NORMAL_BOOT_LOGGING_PROBE_MARKER: &str = "userspace init: native NSWP logging probe passed";
const NORMAL_BOOT_LOGCTL_MARKER: &str = "userspace init: logctl show passed";
const NORMAL_BOOT_BLOCK_DEVICE_MARKER: &str = "userspace init: read-only block-device probe passed";
const NORMAL_BOOT_NULLFS_DISCOVERY_MARKER: &str = "kind=NullFS";
const NORMAL_BOOT_WRITABLE_NULLFS_PARTITION_MARKER: &str =
    "userspace init: writable NullFS partition probe passed";
const NORMAL_BOOT_NULLFS_SERVICE_MARKER: &str = "userspace init: writable NullFS service ready";
const NORMAL_BOOT_NULLFS_READINESS_MARKER: &str = "userspace init: NullFS readiness passed";
const NORMAL_BOOT_VFS_READINESS_MARKER: &str = "userspace init: vfs readiness passed";
const NORMAL_BOOT_SERVICE_CONTROL_MARKER: &str = "userspace init: sv status logging passed";
const NORMAL_BOOT_NULLFS_GENERATION_MARKER: &str = "nullfs ready desired=running generation=1";
const NORMAL_BOOT_TMPFS_GENERATION_MARKER: &str = "tmpfs ready desired=running generation=1";
const NORMAL_BOOT_VFS_GENERATION_MARKER: &str = "vfs ready desired=running generation=1";
const DEFINITION_SERVICE_LOADING_MARKER: &str =
    "userspace init: loading service definition from /System/services";
const DEFINITION_SERVICE_FIRST_FAILURE_MARKER: &str =
    "definition-service-probe: intentional first-generation failure";
const DEFINITION_SERVICE_RESTARTING_MARKER: &str =
    "userspace init: definition-backed service exited; restarting";
const DEFINITION_SERVICE_READY_MARKER: &str = "userspace init: definition-backed service ready";
const DEFINITION_SERVICE_VERIFIED_MARKER: &str =
    "userspace init: definition-backed activation and restart verified";
const NORMAL_BOOT_INIT_SHELL_MARKER: &str = "userspace init launched /ush";
const NORMAL_BOOT_SHELL_MARKER: &str = "userspace shell ready";
const NULLFS_RESTART_MODE_MARKER: &str = "boot mode selected: nullfs-restart-test";
const LOGGING_COLLECTOR_RESTART_PASSED_MARKER: &str = "userspace init: logging collector ring, backpressure, redaction, and route generation isolation verified";
const NULLFS_RESTART_PASSED_MARKER: &str =
    "userspace init: NullFS restart persistent VFS mutation and stale descriptors verified";
const NULLFS_UNAVAILABLE_MODE_MARKER: &str = "boot mode selected: nullfs-unavailable-test";
const NULLFS_UNAVAILABLE_PARTITIONS_MARKER: &str =
    "partition table initialized: kind=MBR, partitions=2,";
const NULLFS_UNAVAILABLE_HANDOFF_MARKER: &str =
    "userspace init: configured primary NullFS volume unavailable; entering recovery";
const NULLFS_UNAVAILABLE_INIT_EXIT_MARKER: &str =
    "userspace process exited: pid=1, path=/init, exit_code=78";
const NULLFS_UNAVAILABLE_INIT_TERMINATED_MARKER: &str =
    "userspace init terminated: pid=1; entering emergency kernel shell";
const EMERGENCY_SHELL_READY_MARKER: &str = "Interactive shell ready. Type `help` for commands.";
const LOGGING_LIFECYCLE_MODE_MARKER: &str = "boot mode selected: logging-lifecycle-test";
const LOGGING_LIFECYCLE_STOPPING_MARKER: &str = "logging stopping desired=stopped generation=1";
const LOGGING_LIFECYCLE_STOPPED_MARKER: &str = "logging stopped desired=running";
const LOGGING_LIFECYCLE_READY_MARKER: &str = "logging ready desired=running generation=2";
const LOGGING_LIFECYCLE_FORCE_TERMINATION_MARKER: &str =
    "userspace init: logging service termination grace expired; forcing exit";
const LOGGING_LIFECYCLE_READINESS_TIMEOUT_MARKER: &str =
    "userspace init: logging service readiness deadline expired; forcing exit";
const LOGGING_LIFECYCLE_PASSED_MARKER: &str = "userspace init: logging live start, stop, route withdrawal, restart fencing, and generation replacement verified";
const NORMAL_BOOT_TIMEOUT: Duration = Duration::from_secs(300);
const SMOKE_PHASE_TIMEOUT: Duration = Duration::from_secs(420);
const NULLFS_RESTART_TEST_TIMEOUT: Duration = Duration::from_secs(420);
const NULLFS_UNAVAILABLE_TEST_TIMEOUT: Duration = Duration::from_secs(120);
const LOGGING_LIFECYCLE_TEST_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
struct NormalBootProgress {
    mode_selected: bool,
    early_log_ready: bool,
    kernel_ready: bool,
    init_ready: bool,
    logging_imported: bool,
    logging_service_ready: bool,
    logging_probe_passed: bool,
    logctl_passed: bool,
    block_device_ready: bool,
    nullfs_partition_discovered: bool,
    writable_nullfs_partition_verified: bool,
    nullfs_service_ready: bool,
    nullfs_readiness_passed: bool,
    vfs_readiness_passed: bool,
    service_control_passed: bool,
    nullfs_generation_verified: bool,
    tmpfs_generation_verified: bool,
    vfs_generation_verified: bool,
    definition_loading_observed: bool,
    definition_first_failure_observed: bool,
    definition_restart_observed: bool,
    definition_ready_observed: bool,
    definition_verified: bool,
    init_launched_shell: bool,
    shell_ready: bool,
}

impl NormalBootProgress {
    fn observe(&mut self, line: &str) -> bool {
        self.mode_selected |= line.contains(NORMAL_BOOT_MODE_MARKER);
        self.early_log_ready |= line.contains(NORMAL_BOOT_EARLY_LOG_MARKER);
        self.kernel_ready |= line.contains(NORMAL_BOOT_READY_MARKER);
        self.init_ready |= line.contains(NORMAL_BOOT_INIT_MARKER);
        self.logging_imported |= line.contains(NORMAL_BOOT_LOGGING_IMPORT_MARKER);
        self.logging_service_ready |= line.contains(NORMAL_BOOT_LOGGING_SERVICE_MARKER);
        self.logging_probe_passed |= line.contains(NORMAL_BOOT_LOGGING_PROBE_MARKER);
        self.logctl_passed |= line.contains(NORMAL_BOOT_LOGCTL_MARKER);
        self.block_device_ready |= line.contains(NORMAL_BOOT_BLOCK_DEVICE_MARKER);
        self.nullfs_partition_discovered |= line.contains(NORMAL_BOOT_NULLFS_DISCOVERY_MARKER);
        self.writable_nullfs_partition_verified |=
            line.contains(NORMAL_BOOT_WRITABLE_NULLFS_PARTITION_MARKER);
        self.nullfs_service_ready |= line.contains(NORMAL_BOOT_NULLFS_SERVICE_MARKER);
        self.nullfs_readiness_passed |= line.contains(NORMAL_BOOT_NULLFS_READINESS_MARKER);
        self.vfs_readiness_passed |= line.contains(NORMAL_BOOT_VFS_READINESS_MARKER);
        self.service_control_passed |= line.contains(NORMAL_BOOT_SERVICE_CONTROL_MARKER);
        self.nullfs_generation_verified |= line.contains(NORMAL_BOOT_NULLFS_GENERATION_MARKER);
        self.tmpfs_generation_verified |= line.contains(NORMAL_BOOT_TMPFS_GENERATION_MARKER);
        self.vfs_generation_verified |= line.contains(NORMAL_BOOT_VFS_GENERATION_MARKER);
        self.definition_loading_observed |= line.contains(DEFINITION_SERVICE_LOADING_MARKER);
        self.definition_first_failure_observed |= self.definition_loading_observed
            && line.contains(DEFINITION_SERVICE_FIRST_FAILURE_MARKER);
        self.definition_restart_observed |= self.definition_first_failure_observed
            && line.contains(DEFINITION_SERVICE_RESTARTING_MARKER);
        self.definition_ready_observed |=
            self.definition_restart_observed && line.contains(DEFINITION_SERVICE_READY_MARKER);
        self.definition_verified |=
            self.definition_ready_observed && line.contains(DEFINITION_SERVICE_VERIFIED_MARKER);
        self.init_launched_shell |= line.contains(NORMAL_BOOT_INIT_SHELL_MARKER);
        self.shell_ready |= line.contains(NORMAL_BOOT_SHELL_MARKER);

        self.mode_selected
            && self.early_log_ready
            && self.kernel_ready
            && self.init_ready
            && self.logging_imported
            && self.logging_service_ready
            && self.logging_probe_passed
            && self.logctl_passed
            && self.block_device_ready
            && self.nullfs_partition_discovered
            && self.writable_nullfs_partition_verified
            && self.nullfs_service_ready
            && self.nullfs_readiness_passed
            && self.vfs_readiness_passed
            && self.service_control_passed
            && self.nullfs_generation_verified
            && self.tmpfs_generation_verified
            && self.vfs_generation_verified
            && self.definition_loading_observed
            && self.definition_first_failure_observed
            && self.definition_restart_observed
            && self.definition_ready_observed
            && self.definition_verified
            && self.init_launched_shell
            && self.shell_ready
    }
}

#[derive(Debug, Default)]
struct UnavailablePrimaryProgress {
    partitions_absent: bool,
    mode_selected: bool,
    init_ready: bool,
    recovery_handoff: bool,
    init_exited: bool,
    init_terminated: bool,
    emergency_shell_ready: bool,
}

impl UnavailablePrimaryProgress {
    fn observe(&mut self, line: &str) -> bool {
        self.partitions_absent |= line.contains(NULLFS_UNAVAILABLE_PARTITIONS_MARKER);
        self.mode_selected |=
            self.partitions_absent && line.contains(NULLFS_UNAVAILABLE_MODE_MARKER);
        self.init_ready |= self.mode_selected && line.contains(NORMAL_BOOT_INIT_MARKER);
        self.recovery_handoff |=
            self.init_ready && line.contains(NULLFS_UNAVAILABLE_HANDOFF_MARKER);
        self.init_exited |= self.recovery_handoff && line == NULLFS_UNAVAILABLE_INIT_EXIT_MARKER;
        self.init_terminated |=
            self.init_exited && line.contains(NULLFS_UNAVAILABLE_INIT_TERMINATED_MARKER);
        self.emergency_shell_ready |=
            self.init_terminated && line.contains(EMERGENCY_SHELL_READY_MARKER);

        self.partitions_absent
            && self.mode_selected
            && self.init_ready
            && self.recovery_handoff
            && self.init_exited
            && self.init_terminated
            && self.emergency_shell_ready
    }
}

#[derive(Debug, Default)]
struct LoggingLifecycleProgress {
    mode_selected: bool,
    stopping_observed: bool,
    stopped_observed: bool,
    replacement_ready: bool,
    force_termination_observed: bool,
    readiness_timeout_observed: bool,
    lifecycle_verified: bool,
}

impl LoggingLifecycleProgress {
    fn observe(&mut self, line: &str) -> bool {
        self.mode_selected |= line.contains(LOGGING_LIFECYCLE_MODE_MARKER);
        self.stopping_observed |= line.contains(LOGGING_LIFECYCLE_STOPPING_MARKER);
        self.stopped_observed |= line.contains(LOGGING_LIFECYCLE_STOPPED_MARKER);
        self.replacement_ready |= line.contains(LOGGING_LIFECYCLE_READY_MARKER);
        self.force_termination_observed |=
            line.contains(LOGGING_LIFECYCLE_FORCE_TERMINATION_MARKER);
        self.readiness_timeout_observed |=
            line.contains(LOGGING_LIFECYCLE_READINESS_TIMEOUT_MARKER);
        if self.mode_selected
            && self.stopping_observed
            && self.stopped_observed
            && self.replacement_ready
            && self.force_termination_observed
            && self.readiness_timeout_observed
            && line.contains(LOGGING_LIFECYCLE_PASSED_MARKER)
        {
            self.lifecycle_verified = true;
        }

        self.lifecycle_verified
    }
}

#[derive(Debug, Default)]
struct Options {
    headless: bool,
    boot_check: bool,
    test: bool,
    nullfs_restart_check: bool,
    nullfs_unavailable_check: bool,
    logging_lifecycle_check: bool,
}

impl Options {
    fn boot_verification_selected(&self) -> bool {
        self.boot_check
            || self.test
            || self.nullfs_restart_check
            || self.nullfs_unavailable_check
            || self.logging_lifecycle_check
    }
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(exit_code) => return exit_code,
    };

    if options.test {
        run_kernel_smoke_test(&options)
    } else if options.nullfs_restart_check {
        run_nullfs_restart_check(&options)
    } else if options.nullfs_unavailable_check {
        run_nullfs_unavailable_check(&options)
    } else if options.logging_lifecycle_check {
        run_logging_lifecycle_check(&options)
    } else if options.boot_check {
        run_normal_boot_check(&options)
    } else {
        run_interactive(qemu_command(&options))
    }
}

fn parse_options() -> Result<Options, ExitCode> {
    parse_options_from(env::args().skip(1))
}

fn parse_options_from(arguments: impl IntoIterator<Item = String>) -> Result<Options, ExitCode> {
    let mut options = Options::default();

    for argument in arguments {
        match argument.as_str() {
            "--headless" => options.headless = true,
            "--boot-check" => {
                if options.boot_verification_selected() {
                    eprintln!("only one boot verification mode may be selected");
                    print_usage();
                    return Err(ExitCode::from(2));
                }
                options.boot_check = true;
                options.headless = true;
            }
            "--test" => {
                if options.boot_verification_selected() {
                    eprintln!("only one boot verification mode may be selected");
                    print_usage();
                    return Err(ExitCode::from(2));
                }
                options.test = true;
                options.headless = true;
            }
            "--nullfs-restart-check" => {
                if options.boot_verification_selected() {
                    eprintln!("only one boot verification mode may be selected");
                    print_usage();
                    return Err(ExitCode::from(2));
                }
                options.nullfs_restart_check = true;
                options.headless = true;
            }
            "--nullfs-unavailable-check" => {
                if options.boot_verification_selected() {
                    eprintln!("only one boot verification mode may be selected");
                    print_usage();
                    return Err(ExitCode::from(2));
                }
                options.nullfs_unavailable_check = true;
                options.headless = true;
            }
            "--logging-lifecycle-check" => {
                if options.boot_verification_selected() {
                    eprintln!("only one boot verification mode may be selected");
                    print_usage();
                    return Err(ExitCode::from(2));
                }
                options.logging_lifecycle_check = true;
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
    println!(
        "Usage: cargo run -- [--headless] [--boot-check | --test | --nullfs-restart-check | --nullfs-unavailable-check | --logging-lifecycle-check]"
    );
    println!("  --headless  Disable the QEMU display and use serial output only");
    println!("  --boot-check  Verify that PID 1 launches the userspace shell");
    println!(
        "  --nullfs-restart-check  Verify NullFS replacement, persistent VFS mutation, and stale descriptors"
    );
    println!(
        "  --nullfs-unavailable-check  Verify missing-primary recovery through the independent emergency shell"
    );
    println!(
        "  --logging-lifecycle-check  Verify logging live start, stop, route withdrawal, and generation replacement"
    );
    println!(
        "  --test      Verify hardware, persistent FAT writes across two boots, VFS, the Rust userspace runtime, transactional exec, copy-on-write fork, process environments, tmpfs, redirection, process control, pipelines, jobs, default signals, and handled signals"
    );
}

fn qemu_command(options: &Options) -> Command {
    qemu_command_for_image(options, Path::new(env!("BIOS_IMAGE")))
}

fn qemu_command_for_image(options: &Options, image: &Path) -> Command {
    let mut command = Command::new("qemu-system-x86_64");

    command
        .args(["-machine", "q35"])
        .arg("-drive")
        .arg(format!(
            "if=none,id=bootdisk,format=raw,file={}",
            image.display()
        ))
        .args(["-device", "ide-hd,drive=bootdisk,bus=ide.0,bootindex=1"])
        .args(["-serial", "stdio", "-monitor", "none", "-m", "128M"]);

    if options.headless {
        command.args(["-display", "none"]);
    }

    if options.boot_verification_selected() {
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

fn run_normal_boot_check(options: &Options) -> ExitCode {
    let mut progress = NormalBootProgress::default();
    let passed = run_qemu_until(
        qemu_command(options),
        "normal boot check",
        NORMAL_BOOT_TIMEOUT,
        move |line| progress.observe(line),
    );
    if passed {
        println!("QEMU normal boot check passed");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmokePhase {
    PreparePersistentFat,
    VerifyCompleteSystem,
}

fn run_kernel_smoke_test(options: &Options) -> ExitCode {
    let source_image = Path::new(env!("SMOKE_TEST_BIOS_IMAGE"));
    let test_image = persistent_test_image_path();
    let _ = fs::remove_file(&test_image);
    if let Err(error) = fs::copy(source_image, &test_image) {
        eprintln!(
            "Could not create persistent FAT test image {} from {}: {error}",
            test_image.display(),
            source_image.display()
        );
        return ExitCode::FAILURE;
    }

    let prepare = run_qemu_phase(
        qemu_command_for_image(options, &test_image),
        SmokePhase::PreparePersistentFat,
    );
    let smoke_result = if prepare {
        run_qemu_phase(
            qemu_command_for_image(options, &test_image),
            SmokePhase::VerifyCompleteSystem,
        )
    } else {
        false
    };
    let restart_result = smoke_result && run_nullfs_restart_test(options);
    let logging_result = restart_result && run_logging_lifecycle_test(options);
    let recovery_result = logging_result && run_nullfs_unavailable_test(options);
    let _ = fs::remove_file(&test_image);
    if recovery_result {
        println!("QEMU kernel smoke test passed");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_nullfs_restart_check(options: &Options) -> ExitCode {
    if run_nullfs_restart_test(options) {
        println!("QEMU NullFS restart fault injection passed");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_nullfs_unavailable_check(options: &Options) -> ExitCode {
    if run_nullfs_unavailable_test(options) {
        println!("QEMU unavailable-primary recovery check passed");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_nullfs_unavailable_test(options: &Options) -> bool {
    let image = Path::new(env!("NULLFS_UNAVAILABLE_TEST_BIOS_IMAGE"));
    let mut progress = UnavailablePrimaryProgress::default();
    run_qemu_until(
        qemu_command_for_image(options, image),
        "unavailable-primary recovery check",
        NULLFS_UNAVAILABLE_TEST_TIMEOUT,
        move |line| progress.observe(line),
    )
}

fn run_logging_lifecycle_check(options: &Options) -> ExitCode {
    if run_logging_lifecycle_test(options) {
        println!("QEMU logging lifecycle check passed");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_logging_lifecycle_test(options: &Options) -> bool {
    let image = Path::new(env!("LOGGING_LIFECYCLE_TEST_BIOS_IMAGE"));
    let mut progress = LoggingLifecycleProgress::default();
    run_qemu_until(
        qemu_command_for_image(options, image),
        "logging lifecycle check",
        LOGGING_LIFECYCLE_TEST_TIMEOUT,
        move |line| progress.observe(line),
    )
}

fn run_nullfs_restart_test(options: &Options) -> bool {
    let image = Path::new(env!("NULLFS_RESTART_TEST_BIOS_IMAGE"));
    let mut mode_selected = false;
    let mut logging_import_count = 0_u8;
    let mut logging_collector_verified = false;
    let mut restart_verified = false;
    let mut definition_loading_observed = false;
    let mut definition_first_failure_observed = false;
    let mut definition_restart_observed = false;
    let mut definition_ready_observed = false;
    let mut definition_verified = false;
    run_qemu_until(
        qemu_command_for_image(options, image),
        "NullFS restart fault injection",
        NULLFS_RESTART_TEST_TIMEOUT,
        move |line| {
            mode_selected |= line.contains(NULLFS_RESTART_MODE_MARKER);
            if line.contains(NORMAL_BOOT_LOGGING_IMPORT_MARKER) {
                logging_import_count = logging_import_count.saturating_add(1);
            }
            logging_collector_verified |= line.contains(LOGGING_COLLECTOR_RESTART_PASSED_MARKER);
            restart_verified |= line.contains(NULLFS_RESTART_PASSED_MARKER);
            definition_loading_observed |=
                restart_verified && line.contains(DEFINITION_SERVICE_LOADING_MARKER);
            definition_first_failure_observed |= definition_loading_observed
                && line.contains(DEFINITION_SERVICE_FIRST_FAILURE_MARKER);
            definition_restart_observed |= definition_first_failure_observed
                && line.contains(DEFINITION_SERVICE_RESTARTING_MARKER);
            definition_ready_observed |=
                definition_restart_observed && line.contains(DEFINITION_SERVICE_READY_MARKER);
            definition_verified |=
                definition_ready_observed && line.contains(DEFINITION_SERVICE_VERIFIED_MARKER);
            mode_selected
                && logging_import_count >= 2
                && logging_collector_verified
                && restart_verified
                && definition_loading_observed
                && definition_first_failure_observed
                && definition_restart_observed
                && definition_ready_observed
                && definition_verified
        },
    )
}

fn persistent_test_image_path() -> PathBuf {
    env::temp_dir().join(format!(
        "nullstar-os-persistent-fat-{}.img",
        std::process::id()
    ))
}

fn run_qemu_phase(command: Command, phase: SmokePhase) -> bool {
    let mut heap_ready = false;
    let mut framebuffer_ready = false;
    let mut acpi_ready = false;
    let mut lapic_timer_ready = false;
    let mut scheduler_ready = false;
    let mut pcie_ready = false;
    let mut ahci_ready = false;
    let mut partitions_ready = false;
    let mut fat_ready = false;
    let mut fat_persistent_ready = false;
    let mut vfs_ready = false;
    let mut elf_ready = false;
    let mut userspace_ready = false;
    let mut user_file_io_ready = false;
    let mut user_rust_runtime_ready = false;
    let mut user_exec_ready = false;
    let mut user_fork_ready = false;
    let mut user_environment_ready = false;
    let mut user_terminal_ready = false;
    let mut user_pipe_ready = false;
    let mut user_shell_ready = false;
    let mut user_pipeline_output_ready = false;
    let mut user_pipeline_ready = false;
    let mut user_background_ready = false;
    let mut user_stopped_job_ready = false;
    let mut user_tmpfs_ready = false;
    let mut user_signal_ready = false;
    let mut user_signal_handler_ready = false;

    let label = match phase {
        SmokePhase::PreparePersistentFat => "persistent FAT preparation",
        SmokePhase::VerifyCompleteSystem => "complete-system smoke test",
    };
    run_qemu_until(command, label, SMOKE_PHASE_TIMEOUT, move |line| {
        if phase == SmokePhase::PreparePersistentFat {
            return line.contains(FAT_PERSIST_PREPARED_MARKER)
                || line.contains(FAT_PERSIST_VERIFIED_MARKER);
        }

        heap_ready |= line.contains(HEAP_TEST_MARKER);
        framebuffer_ready |= line.contains(FRAMEBUFFER_TEST_MARKER);
        acpi_ready |= line.contains(ACPI_TEST_MARKER);
        lapic_timer_ready |= line.contains(LAPIC_TIMER_TEST_MARKER);
        scheduler_ready |= line.contains(SCHEDULER_TEST_MARKER);
        pcie_ready |= line.contains(PCIE_TEST_MARKER);
        ahci_ready |= line.contains(AHCI_TEST_MARKER);
        partitions_ready |= line.contains(PARTITION_TEST_MARKER);
        fat_ready |= line.contains(FAT_TEST_MARKER);
        fat_persistent_ready |= line.contains(FAT_PERSIST_VERIFIED_MARKER);
        vfs_ready |= line.contains(VFS_TEST_MARKER);
        elf_ready |= line.contains(ELF_TEST_MARKER);
        userspace_ready |= line.contains(USERSPACE_TEST_MARKER);
        user_file_io_ready |= line.contains(USER_FILE_IO_TEST_MARKER);
        user_rust_runtime_ready |= line.contains(USER_RUST_RUNTIME_TEST_MARKER);
        user_exec_ready |= line.contains(USER_EXEC_TEST_MARKER);
        user_fork_ready |= line.contains(USER_FORK_TEST_MARKER);
        user_environment_ready |= line.contains(USER_ENVIRONMENT_TEST_MARKER);
        user_terminal_ready |= line.contains(USER_TERMINAL_TEST_MARKER);
        user_pipe_ready |= line.contains(USER_PIPE_TEST_MARKER);
        user_shell_ready |= line.contains(USER_SHELL_TEST_MARKER);
        user_pipeline_output_ready |= line.contains(USER_PIPELINE_OUTPUT_MARKER);
        user_pipeline_ready |= line.contains(USER_PIPELINE_TEST_MARKER);
        user_background_ready |= line.contains(USER_BACKGROUND_TEST_MARKER);
        user_stopped_job_ready |= line.contains(USER_STOPPED_JOB_TEST_MARKER);
        user_tmpfs_ready |= line.contains(USER_TMPFS_TEST_MARKER);
        user_signal_ready |= line.contains(USER_SIGNAL_TEST_MARKER);
        user_signal_handler_ready |= line.contains(USER_SIGNAL_HANDLER_TEST_MARKER);

        heap_ready
            && framebuffer_ready
            && acpi_ready
            && lapic_timer_ready
            && scheduler_ready
            && pcie_ready
            && ahci_ready
            && partitions_ready
            && fat_ready
            && fat_persistent_ready
            && vfs_ready
            && elf_ready
            && userspace_ready
            && user_file_io_ready
            && user_rust_runtime_ready
            && user_exec_ready
            && user_fork_ready
            && user_environment_ready
            && user_terminal_ready
            && user_pipe_ready
            && user_shell_ready
            && user_pipeline_output_ready
            && user_pipeline_ready
            && user_background_ready
            && user_stopped_job_ready
            && user_tmpfs_ready
            && user_signal_ready
            && user_signal_handler_ready
    })
}

fn run_qemu_until(
    mut command: Command,
    label: &'static str,
    timeout: Duration,
    mut completion: impl FnMut(&str) -> bool + Send + 'static,
) -> bool {
    command.stdout(Stdio::piped()).stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = qemu_start_error(error);
            return false;
        }
    };
    let Some(serial_output) = child.stdout.take() else {
        eprintln!("QEMU serial output was not captured");
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    let (marker_sender, marker_receiver) = mpsc::channel();
    let reader = thread::spawn(move || -> io::Result<()> {
        let mut terminal = io::stdout().lock();

        for line in BufReader::new(serial_output).lines() {
            let line = line?;
            writeln!(terminal, "{line}")?;
            terminal.flush()?;

            if completion(&line) {
                let _ = marker_sender.send(());
                break;
            }
        }

        Ok(())
    });

    let deadline = Instant::now() + timeout;
    loop {
        match marker_receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                report_reader_result(reader.join());
                eprintln!("QEMU stopped producing serial output during {label}");
                return false;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                report_reader_result(reader.join());
                eprintln!("QEMU exited with status {status} during {label}");
                return false;
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                report_reader_result(reader.join());
                eprintln!("Could not query QEMU status during {label}: {error}");
                return false;
            }
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            report_reader_result(reader.join());
            eprintln!("QEMU {label} timed out after {} seconds", timeout.as_secs());
            return false;
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

#[cfg(test)]
mod tests {
    use super::{
        DEFINITION_SERVICE_FIRST_FAILURE_MARKER, DEFINITION_SERVICE_LOADING_MARKER,
        DEFINITION_SERVICE_READY_MARKER, DEFINITION_SERVICE_RESTARTING_MARKER,
        DEFINITION_SERVICE_VERIFIED_MARKER, EMERGENCY_SHELL_READY_MARKER,
        LOGGING_LIFECYCLE_FORCE_TERMINATION_MARKER, LOGGING_LIFECYCLE_MODE_MARKER,
        LOGGING_LIFECYCLE_PASSED_MARKER, LOGGING_LIFECYCLE_READINESS_TIMEOUT_MARKER,
        LOGGING_LIFECYCLE_READY_MARKER, LOGGING_LIFECYCLE_STOPPED_MARKER,
        LOGGING_LIFECYCLE_STOPPING_MARKER, LoggingLifecycleProgress,
        NORMAL_BOOT_BLOCK_DEVICE_MARKER, NORMAL_BOOT_EARLY_LOG_MARKER, NORMAL_BOOT_INIT_MARKER,
        NORMAL_BOOT_INIT_SHELL_MARKER, NORMAL_BOOT_LOGCTL_MARKER,
        NORMAL_BOOT_LOGGING_IMPORT_MARKER, NORMAL_BOOT_LOGGING_PROBE_MARKER,
        NORMAL_BOOT_LOGGING_SERVICE_MARKER, NORMAL_BOOT_MODE_MARKER,
        NORMAL_BOOT_NULLFS_DISCOVERY_MARKER, NORMAL_BOOT_NULLFS_GENERATION_MARKER,
        NORMAL_BOOT_NULLFS_READINESS_MARKER, NORMAL_BOOT_NULLFS_SERVICE_MARKER,
        NORMAL_BOOT_READY_MARKER, NORMAL_BOOT_SERVICE_CONTROL_MARKER, NORMAL_BOOT_SHELL_MARKER,
        NORMAL_BOOT_TMPFS_GENERATION_MARKER, NORMAL_BOOT_VFS_GENERATION_MARKER,
        NORMAL_BOOT_VFS_READINESS_MARKER, NORMAL_BOOT_WRITABLE_NULLFS_PARTITION_MARKER,
        NULLFS_UNAVAILABLE_HANDOFF_MARKER, NULLFS_UNAVAILABLE_INIT_EXIT_MARKER,
        NULLFS_UNAVAILABLE_INIT_TERMINATED_MARKER, NULLFS_UNAVAILABLE_MODE_MARKER,
        NULLFS_UNAVAILABLE_PARTITIONS_MARKER, NormalBootProgress, UnavailablePrimaryProgress,
        parse_options_from,
    };

    #[test]
    fn normal_boot_requires_userspace_init_and_shell() {
        let mut progress = NormalBootProgress::default();

        assert!(!progress.observe(NORMAL_BOOT_MODE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_EARLY_LOG_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_READY_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_INIT_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_LOGGING_IMPORT_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_LOGGING_SERVICE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_LOGGING_PROBE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_LOGCTL_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_BLOCK_DEVICE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_NULLFS_DISCOVERY_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_WRITABLE_NULLFS_PARTITION_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_NULLFS_SERVICE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_NULLFS_READINESS_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_VFS_READINESS_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_SERVICE_CONTROL_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_NULLFS_GENERATION_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_TMPFS_GENERATION_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_VFS_GENERATION_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_LOADING_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_FIRST_FAILURE_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_RESTARTING_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_READY_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_VERIFIED_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_INIT_SHELL_MARKER));
        assert!(progress.observe(NORMAL_BOOT_SHELL_MARKER));
    }

    #[test]
    fn normal_boot_rejects_out_of_order_definition_activation_markers() {
        let mut progress = NormalBootProgress::default();

        assert!(!progress.observe(DEFINITION_SERVICE_VERIFIED_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_READY_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_RESTARTING_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_FIRST_FAILURE_MARKER));
        assert!(!progress.definition_verified);
        assert!(!progress.definition_ready_observed);
        assert!(!progress.definition_restart_observed);
        assert!(!progress.definition_first_failure_observed);

        assert!(!progress.observe(DEFINITION_SERVICE_LOADING_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_FIRST_FAILURE_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_RESTARTING_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_READY_MARKER));
        assert!(!progress.observe(DEFINITION_SERVICE_VERIFIED_MARKER));
        assert!(progress.definition_verified);
    }

    #[test]
    fn kernel_readiness_alone_does_not_complete_normal_boot() {
        let mut progress = NormalBootProgress::default();

        assert!(!progress.observe(NORMAL_BOOT_MODE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_EARLY_LOG_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_READY_MARKER));
        assert!(!progress.observe("Interactive shell ready"));
    }

    #[test]
    fn unavailable_primary_requires_ordered_recovery_handoff() {
        let mut progress = UnavailablePrimaryProgress::default();

        assert!(!progress.observe(EMERGENCY_SHELL_READY_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_INIT_TERMINATED_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_INIT_EXIT_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_HANDOFF_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_INIT_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_MODE_MARKER));
        assert!(
            !progress.observe(
                "partition table initialized: kind=MBR, partitions=20, protective_mbr=false"
            )
        );
        assert!(!progress.observe(NULLFS_UNAVAILABLE_PARTITIONS_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_MODE_MARKER));
        assert!(!progress.observe(NORMAL_BOOT_INIT_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_HANDOFF_MARKER));
        assert!(!progress.observe("userspace process exited: pid=1, path=/init, exit_code=780"));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_INIT_EXIT_MARKER));
        assert!(!progress.observe(NULLFS_UNAVAILABLE_INIT_TERMINATED_MARKER));
        assert!(progress.observe(EMERGENCY_SHELL_READY_MARKER));
    }

    #[test]
    fn logging_lifecycle_requires_mode_transitions_and_final_marker() {
        let mut progress = LoggingLifecycleProgress::default();

        assert!(!progress.observe(LOGGING_LIFECYCLE_PASSED_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_MODE_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_STOPPING_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_STOPPED_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_READY_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_PASSED_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_FORCE_TERMINATION_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_PASSED_MARKER));
        assert!(!progress.observe(LOGGING_LIFECYCLE_READINESS_TIMEOUT_MARKER));
        assert!(progress.observe(LOGGING_LIFECYCLE_PASSED_MARKER));
    }

    #[test]
    fn nullfs_unavailable_option_is_headless() {
        let options = parse_options_from(["--nullfs-unavailable-check".to_owned()])
            .expect("NullFS unavailable option should parse");

        assert!(options.nullfs_unavailable_check);
        assert!(options.headless);
    }

    #[test]
    fn logging_lifecycle_option_is_headless() {
        let options = parse_options_from(["--logging-lifecycle-check".to_owned()])
            .expect("logging lifecycle option should parse");

        assert!(options.logging_lifecycle_check);
        assert!(options.headless);
    }

    #[test]
    fn boot_verification_modes_are_mutually_exclusive() {
        let modes = [
            "--boot-check",
            "--test",
            "--nullfs-restart-check",
            "--nullfs-unavailable-check",
            "--logging-lifecycle-check",
        ];

        for (index, first) in modes.iter().enumerate() {
            for second in &modes[index + 1..] {
                assert!(
                    parse_options_from([(*first).to_owned(), (*second).to_owned()]).is_err(),
                    "{first} and {second} must be mutually exclusive"
                );
            }
        }
    }
}
