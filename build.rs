use std::env;
use std::fs;
use std::path::PathBuf;

const HELLO_TEXT: &str = "Hello from a GalacticOS userspace file descriptor.\n";

fn main() {
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR was not set"));

    let kernel_binary = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_KERNEL_kernel").expect("kernel artifact path was not set"),
    );
    let userspace_init = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_init")
            .expect("userspace init artifact path was not set"),
    );
    let userspace_fault_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_fault_probe")
            .expect("userspace fault-probe artifact path was not set"),
    );
    let userspace_cat = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_cat")
            .expect("userspace cat artifact path was not set"),
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
    let userspace_shell = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_ush")
            .expect("userspace shell artifact path was not set"),
    );
    let hello_text = output_directory.join("hello.txt");
    fs::write(&hello_text, HELLO_TEXT).expect("failed to create userspace file-I/O fixture");

    let bios_image = output_directory.join("galactic-os-bios.img");
    let mut image = bootloader::DiskImageBuilder::new(kernel_binary);
    image.set_file(String::from("init"), userspace_init);
    image.set_file(String::from("fault-probe"), userspace_fault_probe);
    image.set_file(String::from("cat"), userspace_cat);
    image.set_file(String::from("readline"), userspace_readline);
    image.set_file(String::from("pipe-producer"), userspace_pipe_producer);
    image.set_file(String::from("pipe-consumer"), userspace_pipe_consumer);
    image.set_file(String::from("upper"), userspace_upper);
    image.set_file(String::from("delay"), userspace_delay);
    image.set_file(String::from("signal-probe"), userspace_signal_probe);
    image.set_file(String::from("runtime-probe"), userspace_runtime_probe);
    image.set_file(String::from("stderr-probe"), userspace_stderr_probe);
    image.set_file(String::from("exec"), userspace_exec);
    image.set_file(String::from("exec-source"), userspace_exec_source);
    image.set_file(String::from("exec-target"), userspace_exec_target);
    image.set_file(String::from("fork-probe"), userspace_fork_probe);
    image.set_file(String::from("fork-target"), userspace_fork_target);
    image.set_file(
        String::from("signal-handler-probe"),
        userspace_signal_handler_probe,
    );
    image.set_file(
        String::from("signal-lifecycle-probe"),
        userspace_signal_lifecycle_probe,
    );
    image.set_file(
        String::from("signal-lifecycle-target"),
        userspace_signal_lifecycle_target,
    );
    image.set_file(String::from("ush"), userspace_shell);
    image.set_file(String::from("hello.txt"), hello_text);
    image
        .create_bios_image(&bios_image)
        .expect("failed to create BIOS disk image");

    println!("cargo:rustc-env=BIOS_IMAGE={}", bios_image.display());
}
