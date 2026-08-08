use service_definition::{Readiness, RestartPolicy, SERVICE_DEFINITION_HEADER, ServiceId, parse};

const DEFINITION: &[u8] = b"NullStar Service Definition 1\n\
ServiceId=4c71a3aa-bc2c-4b38-8db4-737e0369ef8c\n\
Name=system.definition-probe\n\
Description=Definition-backed activation probe\n\
Executable=/System/bin/service-definition-probe\n\
Argument=--mode\n\
Argument=service activation\n\
Readiness=notify\n\
ReadyMessage=service-definition-probe: ready\n\
Restart=on-failure\n\
RestartLimit=3\n\
RestartBackoffYields=32\n";

#[test]
fn parses_complete_notify_definition() {
    let definition = parse(DEFINITION).unwrap();
    assert_eq!(SERVICE_DEFINITION_HEADER, "NullStar Service Definition 1");
    assert_eq!(
        definition.service_id(),
        ServiceId::from_bytes([
            0x4c, 0x71, 0xa3, 0xaa, 0xbc, 0x2c, 0x4b, 0x38, 0x8d, 0xb4, 0x73, 0x7e, 0x03, 0x69,
            0xef, 0x8c,
        ])
        .unwrap()
    );
    assert_eq!(definition.name(), "system.definition-probe");
    assert_eq!(
        definition.description(),
        "Definition-backed activation probe"
    );
    assert_eq!(
        definition.executable(),
        "/System/bin/service-definition-probe"
    );
    let mut arguments = definition.arguments();
    assert_eq!(arguments.next(), Some("--mode"));
    assert_eq!(arguments.next(), Some("service activation"));
    assert_eq!(arguments.next(), None);
    assert_eq!(definition.readiness(), Readiness::Notify);
    assert_eq!(
        definition.ready_message(),
        Some("service-definition-probe: ready")
    );
    assert_eq!(definition.restart_policy(), RestartPolicy::OnFailure);
    assert_eq!(definition.restart_limit(), 3);
    assert_eq!(definition.restart_backoff_yields(), 32);
}

#[test]
fn comments_and_blank_lines_do_not_change_semantics() {
    let input = b"NullStar Service Definition 1\n\
\n\
# Installed by the deterministic build fixture.\n\
ServiceId=4c71a3aa-bc2c-4b38-8db4-737e0369ef8c\n\
Name=system.definition-probe\n\
Description=Definition-backed activation probe\n\
Executable=/System/bin/service-definition-probe\n\
Readiness=immediate\n\
Restart=never\n\
RestartLimit=0\n\
RestartBackoffYields=0\n";
    let definition = parse(input).unwrap();
    assert_eq!(definition.readiness(), Readiness::Immediate);
    assert_eq!(definition.ready_message(), None);
    assert_eq!(definition.restart_policy(), RestartPolicy::Never);
    assert_eq!(definition.arguments().len(), 0);
}
