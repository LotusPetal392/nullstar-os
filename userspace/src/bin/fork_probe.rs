#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU64, Ordering};
use userspace::{
    ipc::{self, ObjectKind, Rights},
    managed_startup::ManagedToolCommand,
    syscall::{self, OpenFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const OUTPUT_PATH: &[u8] = b"/tmp/fork-shared.txt";
const CHILD_EXEC: &[u8] = b"/fork-target inherited";
const CAPABILITY_MESSAGE: &[u8] = b"child-capability-channel";
const PARENT_VALUE: u64 = 0x1122_3344_5566_7788;
const CHILD_VALUE: u64 = 0x8877_6655_4433_2211;

static FORK_CELL: AtomicU64 = AtomicU64::new(0);

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let descriptor = match syscall::open(
        OUTPUT_PATH,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
    ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(1),
    };
    if descriptor != 3 {
        syscall::exit(2);
    }
    let endpoint = match ipc::endpoint_create() {
        Ok(endpoint) => endpoint,
        Err(_) => syscall::exit(12),
    };

    let mut stack_page = [0_u8; 4096];
    stack_page[0] = 0x31;
    stack_page[4095] = 0x32;
    FORK_CELL.store(PARENT_VALUE, Ordering::SeqCst);

    let child = match syscall::fork() {
        Ok(process_id) => process_id,
        Err(_) => syscall::exit(3),
    };

    if child == 0 {
        let endpoint_info = match ipc::wait_for_handle(endpoint) {
            Ok(info) => info,
            Err(_) => syscall::exit(13),
        };
        if endpoint_info.kind != ObjectKind::Endpoint || endpoint_info.rights != Rights::SEND {
            syscall::exit(14);
        }
        if ipc::send(endpoint, CAPABILITY_MESSAGE, None).is_err() {
            syscall::exit(15);
        }

        FORK_CELL.store(CHILD_VALUE, Ordering::SeqCst);
        stack_page[0] = 0x41;
        stack_page[4095] = 0x42;
        if FORK_CELL.load(Ordering::SeqCst) != CHILD_VALUE
            || stack_page[0] != 0x41
            || stack_page[4095] != 0x42
        {
            syscall::exit(4);
        }
        if syscall::write_all(descriptor, b"child-before-exec\n").is_err() {
            syscall::exit(5);
        }
        if syscall::close_all_capabilities().is_err()
            || syscall::exec_managed_command(ManagedToolCommand::new(CHILD_EXEC, &[])).is_err()
        {
            syscall::exit(6);
        }
        syscall::exit(7)
    }

    if ipc::grant_child(child, endpoint, Rights::SEND, endpoint).ok() != Some(endpoint) {
        syscall::exit(16);
    }
    let mut capability_message = [0_u8; 32];
    let received = match ipc::receive(endpoint, &mut capability_message) {
        Ok(message) => message,
        Err(_) => syscall::exit(18),
    };
    if received.sender_process_id != child
        || received.bytes != CAPABILITY_MESSAGE.len()
        || received.capability.is_some()
        || &capability_message[..received.bytes] != CAPABILITY_MESSAGE
    {
        syscall::exit(19);
    }

    let status = match syscall::wait_child(child) {
        Ok(status) => status,
        Err(_) => syscall::exit(8),
    };
    if status.raw() != 17
        || FORK_CELL.load(Ordering::SeqCst) != PARENT_VALUE
        || stack_page[0] != 0x31
        || stack_page[4095] != 0x32
    {
        syscall::exit(9);
    }
    if ipc::close(endpoint).is_err() {
        syscall::exit(20);
    }
    if syscall::write_all(descriptor, b"parent-after-wait\n").is_err() {
        syscall::exit(10);
    }
    if syscall::close(descriptor).is_err() {
        syscall::exit(11);
    }
    syscall::exit(0)
}
