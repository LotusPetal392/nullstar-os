#![no_std]
#![no_main]

use core::arch::asm;

use userspace::syscall::{self, STDOUT};

userspace::entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::write_all(STDOUT, b"userspace: hello from ring 3\n").is_err() {
        syscall::exit(1);
    }

    unsafe {
        asm!(
            "mov rcx, 50000000",
            "2:",
            "pause",
            "dec rcx",
            "jnz 2b",
            out("rcx") _,
            options(nomem, nostack),
        );
    }

    if syscall::yield_now().is_err()
        || syscall::write_all(STDOUT, b"userspace: resumed after yield\n").is_err()
    {
        syscall::exit(1);
    }
    syscall::exit(42)
}
