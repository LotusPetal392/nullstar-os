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
    let hello_text = output_directory.join("hello.txt");
    fs::write(&hello_text, HELLO_TEXT).expect("failed to create userspace file-I/O fixture");

    let bios_image = output_directory.join("galactic-os-bios.img");
    let mut image = bootloader::DiskImageBuilder::new(kernel_binary);
    image.set_file(String::from("init"), userspace_init);
    image.set_file(String::from("fault-probe"), userspace_fault_probe);
    image.set_file(String::from("cat"), userspace_cat);
    image.set_file(String::from("hello.txt"), hello_text);
    image
        .create_bios_image(&bios_image)
        .expect("failed to create BIOS disk image");

    println!("cargo:rustc-env=BIOS_IMAGE={}", bios_image.display());
}
