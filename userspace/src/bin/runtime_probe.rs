#![no_std]
#![no_main]

use userspace::{
    args::Args,
    heap::BumpHeap,
    syscall::{self, STDOUT},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const EXPECTED_ARGUMENT: &[u8] = b"runtime-smoke";
const SUCCESS: &[u8] = b"userspace Rust runtime probe passed\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 2 || arguments.get(1) != Some(EXPECTED_ARGUMENT) {
        syscall::exit(64);
    }

    let mut heap = BumpHeap::<4096>::new();
    {
        let Some(block) = heap.allocate(257, 16) else {
            syscall::exit(1);
        };
        if block.as_ptr() as usize % 16 != 0 {
            syscall::exit(1);
        }
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (index & 0xff) as u8;
        }
        if block[0] != 0 || block[256] != 0 {
            syscall::exit(1);
        }

        let Some(copy) = heap.copy_bytes(EXPECTED_ARGUMENT, 8) else {
            syscall::exit(1);
        };
        if copy != EXPECTED_ARGUMENT || heap.used() <= block.len() {
            syscall::exit(1);
        }
    }

    heap.reset();
    if heap.used() != 0 || heap.remaining() != heap.capacity() {
        syscall::exit(1);
    }
    if syscall::getpid().is_err() || syscall::write_all(STDOUT, SUCCESS).is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}
