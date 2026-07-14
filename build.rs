use std::env;
use std::path::PathBuf;

fn main() {
    let output_directory = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR was not set"));

    let kernel_binary = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_KERNEL_kernel").expect("kernel artifact path was not set"),
    );

    let bios_image = output_directory.join("galactic-os-bios.img");

    bootloader::BiosBoot::new(&kernel_binary)
        .create_disk_image(&bios_image)
        .expect("failed to create BIOS disk image");

    println!("cargo:rustc-env=BIOS_IMAGE={}", bios_image.display());
}
