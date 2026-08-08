pub const SERVICE_ID_BYTES: [u8; 16] = [
    0x4c, 0x71, 0xa3, 0xaa, 0xbc, 0x2c, 0x4b, 0x38, 0x8d, 0xb4, 0x73, 0x7e, 0x03, 0x69, 0xef,
    0x8c,
];
pub const SERVICE_NAME: &str = "system.definition-probe";
pub const DEFINITION_PATH: &[u8] = b"/System/services/definition-probe.service";
pub const EXECUTABLE_PATH: &[u8] = b"/System/bin/definition-service-probe";
pub const READY_MESSAGE: &[u8] = b"service-ready: definition-probe";
pub const RESTART_LIMIT: u32 = 3;
pub const RESTART_BACKOFF_YIELDS: u32 = 32;
pub const DEFINITION_BYTES: &[u8] = b"NullStar Service Definition 1\n\
ServiceId=4c71a3aa-bc2c-4b38-8db4-737e0369ef8c\n\
Name=system.definition-probe\n\
Description=Definition-backed activation probe\n\
Executable=/System/bin/definition-service-probe\n\
Readiness=notify\n\
ReadyMessage=service-ready: definition-probe\n\
Restart=on-failure\n\
RestartLimit=3\n\
RestartBackoffYields=32\n";
