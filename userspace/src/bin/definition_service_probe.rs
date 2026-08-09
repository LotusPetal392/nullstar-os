#![no_std]
#![no_main]

use userspace::{
    abi::INIT_PROCESS_ID,
    args::Args,
    definition_service_probe,
    ipc::{self, ObjectKind, Rights},
    platform,
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
        let group_ready = match syscall::pipe_pair() {
            Ok(pair) => pair,
            Err(_) => syscall::exit(6),
        };
        match syscall::fork() {
            Ok(0) => {
                let _ = syscall::close(group_ready.reader);
                if platform::set_process_group(0, 0).is_err()
                    || syscall::write_all(group_ready.writer, &[1]).is_err()
                    || syscall::close(group_ready.writer).is_err()
                {
                    syscall::exit(7);
                }
                loop {
                    if syscall::yield_now().is_err() {
                        syscall::exit(8);
                    }
                }
            }
            Ok(_) => {
                let _ = syscall::close(group_ready.writer);
                let mut ready = [0_u8; 1];
                let escaped = loop {
                    match syscall::read(group_ready.reader, &mut ready) {
                        Ok(1) => break ready[0] == 1,
                        Ok(_) => break false,
                        Err(error) if error == syscall::Errno::INTERRUPTED => {}
                        Err(_) => break false,
                    }
                };
                if syscall::close(group_ready.reader).is_err() || !escaped {
                    syscall::exit(9);
                }
            }
            Err(_) => {
                let _ = syscall::close(group_ready.writer);
                let _ = syscall::close(group_ready.reader);
                syscall::exit(6);
            }
        }
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
