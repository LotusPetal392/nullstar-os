#![no_std]
#![no_main]

use userspace::{
    abi::signal,
    syscall::{self, SignalAction, SignalActionFlags, SignalFrame, SignalMask, SignalMaskHow},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

extern "C" fn inherited_handler(_signal: u64, _frame: *const SignalFrame) {}

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let action = SignalAction::handler(
        inherited_handler,
        SignalMask::from_signal(signal::INTERRUPT),
        SignalActionFlags::RESET_HANDLER,
    );
    if syscall::signal_action(signal::TERMINATE, Some(&action), None).is_err()
        || syscall::signal_mask(
            SignalMaskHow::Block,
            SignalMask::from_signal(signal::INTERRUPT),
        )
        .is_err()
    {
        syscall::exit(1);
    }

    let child = match syscall::fork() {
        Ok(child) => child,
        Err(_) => syscall::exit(2),
    };
    if child == 0 {
        let inherited = match syscall::query_signal_action(signal::TERMINATE) {
            Ok(action) => action,
            Err(_) => syscall::exit(3),
        };
        let mask = match syscall::current_signal_mask() {
            Ok(mask) => mask,
            Err(_) => syscall::exit(4),
        };
        if inherited.handler_address() != inherited_handler as usize as u64
            || !inherited.mask().contains(signal::INTERRUPT)
            || inherited.flags() != SignalActionFlags::RESET_HANDLER
            || !mask.contains(signal::INTERRUPT)
        {
            syscall::exit(5);
        }
        if syscall::execve(b"/signal-lifecycle-target inherited").is_err() {
            syscall::exit(6);
        }
        syscall::exit(7)
    }

    match syscall::wait_child(child) {
        Ok(status) if status.raw() == 19 => syscall::exit(0),
        _ => syscall::exit(8),
    }
}
