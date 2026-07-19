#![no_std]
#![no_main]

use userspace::syscall::{self, STDOUT};

userspace::entry!(rust_main);
userspace::panic_handler!();

// This process deliberately faults so the kernel can verify per-process exception isolation.
extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let _ = syscall::write_all(STDOUT, b"fault-probe: touching an unmapped page now\n");
    unsafe {
        core::ptr::read_volatile(0x0000_0000_dead_0000 as *const u64);
    }
    syscall::exit(99)
}
