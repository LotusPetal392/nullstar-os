#![no_std]
#![no_main]

use userspace::{
    abi::signal,
    args::Args,
    syscall::{self, SignalAction, SignalMask},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 2 || arguments.get(1) != Some(b"inherited") {
        syscall::exit(10);
    }
    let action = match syscall::query_signal_action(signal::TERMINATE) {
        Ok(action) => action,
        Err(_) => syscall::exit(11),
    };
    let mask = match syscall::current_signal_mask() {
        Ok(mask) => mask,
        Err(_) => syscall::exit(12),
    };
    if action != SignalAction::DEFAULT
        || action.mask() != SignalMask::EMPTY
        || !mask.contains(signal::INTERRUPT)
    {
        syscall::exit(13);
    }
    if syscall::write_all(syscall::STDOUT, b"signal lifecycle target passed\n").is_err() {
        syscall::exit(14);
    }
    syscall::exit(19)
}
