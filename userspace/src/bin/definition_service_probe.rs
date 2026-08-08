#![no_std]
#![no_main]

use userspace::{
    abi::INIT_PROCESS_ID,
    args::Args,
    definition_service_probe,
    ipc::{self, ObjectKind, Rights},
    service_route::receive_service_generation,
    syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const READY_HANDLE: u64 = 1;
const GENERATION_HANDOFF_HANDLE: u64 = 5;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 1 || arguments.get(0) != Some(definition_service_probe::EXECUTABLE_PATH) {
        syscall::exit(2);
    }
    if !matches!(
        ipc::wait_for_handle(READY_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) || [2, 3, 4, 6]
        .into_iter()
        .any(|handle| ipc::info(handle).is_ok())
    {
        syscall::exit(3);
    }
    let generation = match receive_service_generation(GENERATION_HANDOFF_HANDLE, INIT_PROCESS_ID) {
        Ok(generation) => generation,
        Err(_) => syscall::exit(4),
    };
    if generation.get() == 1 {
        let _ = syscall::write_all(
            syscall::STDOUT,
            b"definition-service-probe: intentional first-generation failure\n",
        );
        syscall::exit(75);
    }
    if ipc::send(READY_HANDLE, definition_service_probe::READY_MESSAGE, None).is_err()
        || ipc::close(READY_HANDLE).is_err()
        || ipc::info(GENERATION_HANDOFF_HANDLE).is_ok()
        || syscall::write_all(
            syscall::STDOUT,
            b"definition-service-probe: definition-backed generation ready\n",
        )
        .is_err()
    {
        syscall::exit(5);
    }

    loop {
        if syscall::yield_now().is_err() {
            syscall::exit(6);
        }
    }
}
