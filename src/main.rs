use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let bios_image = env!("BIOS_IMAGE");

    let status = Command::new("qemu-system-x86_64")
        .args([
            "-drive",
            &format!("format=raw,file={bios_image}"),
            "-no-reboot",
            "-no-shutdown",
        ])
        .status();

    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,

        Ok(status) => {
            eprintln!("QEMU exited with status: {status}");
            ExitCode::FAILURE
        }

        Err(error) => {
            eprintln!("Could not start QEMU: {error}");
            eprintln!("Make sure qemu-system-x86_64 is installed.");
            ExitCode::FAILURE
        }
    }
}
