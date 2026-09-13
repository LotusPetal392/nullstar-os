#![no_std]
#![no_main]

use userspace::{
    driver_service::run_provisional_driver_service,
    managed_startup::{ManagedServiceIdentity, numeric_service_id},
    service_control::CONSOLE_SERVICE_ID,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const EXECUTABLE_ID: u64 = 7;
const SERVICE_IDENTITY: ManagedServiceIdentity = ManagedServiceIdentity::new(
    EXECUTABLE_ID,
    numeric_service_id(CONSOLE_SERVICE_ID.into_bytes()),
    EXECUTABLE_ID,
);

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    unsafe {
        run_provisional_driver_service(initial_stack, SERVICE_IDENTITY, b"service-ready: console")
    }
}
