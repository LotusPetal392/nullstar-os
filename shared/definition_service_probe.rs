pub const SERVICE_ID_BYTES: [u8; 16] = [
    0x4c, 0x71, 0xa3, 0xaa, 0xbc, 0x2c, 0x4b, 0x38, 0x8d, 0xb4, 0x73, 0x7e, 0x03, 0x69, 0xef,
    0x8c,
];
pub const SERVICE_NAME: &str = "system.definition-probe";
pub const SERVICE_NUMERIC_ID: u64 = u64::from_le_bytes([
    SERVICE_ID_BYTES[0],
    SERVICE_ID_BYTES[1],
    SERVICE_ID_BYTES[2],
    SERVICE_ID_BYTES[3],
    SERVICE_ID_BYTES[4],
    SERVICE_ID_BYTES[5],
    SERVICE_ID_BYTES[6],
    SERVICE_ID_BYTES[7],
]);
pub const SYSTEM_PACKAGE_ID: u64 = 1;
pub const EXECUTABLE_ID: u64 = 1;
pub const COMPONENT_ID: u64 = 1;
pub const NAMESPACE_PROFILE_ID: u64 = 1;
pub const DEFINITION_PATH: &[u8] = b"/System/services/definition-probe.service";
pub const EXECUTABLE_PATH: &[u8] = b"/System/bin/definition-service-probe";
pub const MANAGED_ARGUMENT: &[u8] = b"--managed-bootstrap";
pub const READY_MESSAGE: &[u8] = b"service-ready: definition-probe";
pub const RESTART_LIMIT: u32 = 3;
pub const RESTART_BACKOFF_YIELDS: u32 = 32;
pub const DEFINITION_BYTES: &[u8] = b"NullStar Service Definition 1\n\
ServiceId=4c71a3aa-bc2c-4b38-8db4-737e0369ef8c\n\
Name=system.definition-probe\n\
Description=Definition-backed activation probe\n\
Executable=/System/bin/definition-service-probe\n\
Argument=--managed-bootstrap\n\
Readiness=notify\n\
ReadyMessage=service-ready: definition-probe\n\
Restart=on-failure\n\
RestartLimit=3\n\
RestartBackoffYields=32\n";
