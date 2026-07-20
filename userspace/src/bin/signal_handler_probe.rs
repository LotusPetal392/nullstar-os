#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU64, Ordering};
use userspace::{
    abi::{signal, signal_action},
    syscall::{
        self, Errno, SignalAction, SignalActionFlags, SignalFrame, SignalMask, SignalMaskHow,
    },
};

userspace::entry!(rust_main);
userspace::panic_handler!();

static INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static TERMINATIONS: AtomicU64 = AtomicU64::new(0);
static FRAME_ERRORS: AtomicU64 = AtomicU64::new(0);

extern "C" fn handler(signal_number: u64, frame: *const SignalFrame) {
    let frame = unsafe { frame.as_ref() };
    if !frame.is_some_and(|frame| {
        frame.magic == signal_action::FRAME_MAGIC && frame.signal == signal_number
    }) {
        FRAME_ERRORS.fetch_add(1, Ordering::SeqCst);
        return;
    }
    match signal_number {
        signal::INTERRUPT => {
            INTERRUPTS.fetch_add(1, Ordering::SeqCst);
            let _ = syscall::write_all(syscall::STDOUT, b"handled SIGINT\n");
        }
        signal::TERMINATE => {
            TERMINATIONS.fetch_add(1, Ordering::SeqCst);
            let _ = syscall::write_all(syscall::STDOUT, b"handled SIGTERM\n");
        }
        _ => {
            FRAME_ERRORS.fetch_add(1, Ordering::SeqCst);
        }
    }
}

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let action = SignalAction::handler(handler, SignalMask::EMPTY, SignalActionFlags::EMPTY);
    if syscall::signal_action(signal::INTERRUPT, Some(&action), None).is_err()
        || syscall::signal_action(signal::TERMINATE, Some(&action), None).is_err()
    {
        syscall::exit(1);
    }
    if syscall::signal_mask(
        SignalMaskHow::Block,
        SignalMask::from_signal(signal::TERMINATE),
    )
    .is_err()
    {
        syscall::exit(2);
    }
    if syscall::write_all(syscall::STDOUT, b"signal handler probe ready\n").is_err() {
        syscall::exit(3);
    }

    let mut input = [0_u8; 16];
    match syscall::read(syscall::STDIN, &mut input) {
        Err(error) if error == Errno::INTERRUPTED => {}
        _ => syscall::exit(4),
    }
    if INTERRUPTS.load(Ordering::SeqCst) != 1
        || TERMINATIONS.load(Ordering::SeqCst) != 0
        || FRAME_ERRORS.load(Ordering::SeqCst) != 0
    {
        syscall::exit(5);
    }

    if syscall::signal_mask(
        SignalMaskHow::Unblock,
        SignalMask::from_signal(signal::TERMINATE),
    )
    .is_err()
    {
        syscall::exit(6);
    }
    for _ in 0..256 {
        if TERMINATIONS.load(Ordering::SeqCst) == 1 {
            break;
        }
        if syscall::yield_now().is_err() {
            syscall::exit(7);
        }
    }
    if INTERRUPTS.load(Ordering::SeqCst) != 1
        || TERMINATIONS.load(Ordering::SeqCst) != 1
        || FRAME_ERRORS.load(Ordering::SeqCst) != 0
    {
        syscall::exit(8);
    }
    if syscall::current_signal_mask().map_or(true, |mask| {
        mask.contains(signal::INTERRUPT) || mask.contains(signal::TERMINATE)
    }) {
        syscall::exit(9);
    }
    if syscall::write_all(syscall::STDOUT, b"userspace handled signals passed\n").is_err() {
        syscall::exit(10);
    }
    syscall::exit(0)
}
