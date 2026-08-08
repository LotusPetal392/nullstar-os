use service_definition::{
    Field, MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_DEFINITION_BYTES, MAX_RESTART_BACKOFF_YIELDS,
    ParseError, ServiceIdTextError, parse,
};

fn definition_with(replacement: (&str, &str)) -> Vec<u8> {
    let source = "NullStar Service Definition 1\n\
ServiceId=4c71a3aa-bc2c-4b38-8db4-737e0369ef8c\n\
Name=system.definition-probe\n\
Description=Definition-backed activation probe\n\
Executable=/System/bin/service-definition-probe\n\
Readiness=notify\n\
ReadyMessage=service-definition-probe: ready\n\
Restart=on-failure\n\
RestartLimit=3\n\
RestartBackoffYields=32\n";
    source.replace(replacement.0, replacement.1).into_bytes()
}

#[test]
fn rejects_envelope_and_line_shape_errors() {
    assert_eq!(parse(b""), Err(ParseError::Empty));
    assert_eq!(
        parse(&vec![b'x'; MAX_DEFINITION_BYTES + 1]),
        Err(ParseError::TooLarge)
    );
    assert_eq!(
        parse(b"NullStar Service Definition 1"),
        Err(ParseError::MissingFinalNewline)
    );
    assert_eq!(
        parse(b"NullStar Service Definition 1\r\n"),
        Err(ParseError::CarriageReturn)
    );
    assert_eq!(
        parse(b"NullStar Service Definition 2\n"),
        Err(ParseError::InvalidHeader)
    );
    assert_eq!(
        parse(&definition_with(("Description=", "Description"))),
        Err(ParseError::InvalidLine { line: 4 })
    );
    assert_eq!(
        parse(&definition_with((
            "Description=Definition-backed activation probe\n",
            "Unknown=value\nDescription=Definition-backed activation probe\n",
        ))),
        Err(ParseError::UnknownField { line: 4 })
    );
}

#[test]
fn rejects_missing_duplicate_and_noncanonical_identity_fields() {
    assert_eq!(
        parse(&definition_with((
            "Description=Definition-backed activation probe\n",
            "",
        ))),
        Err(ParseError::MissingField(Field::Description))
    );
    assert_eq!(
        parse(&definition_with((
            "Name=system.definition-probe\n",
            "Name=system.definition-probe\nName=system.other\n",
        ))),
        Err(ParseError::DuplicateField(Field::Name))
    );
    assert_eq!(
        parse(&definition_with((
            "4c71a3aa-bc2c-4b38-8db4-737e0369ef8c",
            "4C71A3AA-BC2C-4B38-8DB4-737E0369EF8C",
        ))),
        Err(ParseError::InvalidServiceId(
            ServiceIdTextError::NonCanonical
        ))
    );
    assert_eq!(
        parse(&definition_with((
            "4c71a3aa-bc2c-4b38-8db4-737e0369ef8c",
            "00000000-0000-0000-0000-000000000000",
        ))),
        Err(ParseError::InvalidServiceId(ServiceIdTextError::Nil))
    );
    assert_eq!(
        parse(&definition_with((
            "Name=system.definition-probe",
            "Name=System.definition-probe",
        ))),
        Err(ParseError::InvalidName)
    );
}

#[test]
fn rejects_invalid_paths_arguments_and_readiness_contracts() {
    assert_eq!(
        parse(&definition_with((
            "/System/bin/service-definition-probe",
            "/System/../service-definition-probe",
        ))),
        Err(ParseError::InvalidExecutable)
    );
    assert_eq!(
        parse(&definition_with((
            "Readiness=notify\nReadyMessage=service-definition-probe: ready",
            "Readiness=immediate\nReadyMessage=service-definition-probe: ready",
        ))),
        Err(ParseError::InconsistentReadiness)
    );
    assert_eq!(
        parse(&definition_with((
            "ReadyMessage=service-definition-probe: ready\n",
            "",
        ))),
        Err(ParseError::InconsistentReadiness)
    );

    let mut too_many = definition_with(("Readiness=notify\n", ""));
    let insertion = (0..=MAX_ARGUMENTS)
        .map(|_| "Argument=value\n")
        .collect::<String>();
    let marker = b"ReadyMessage=";
    let position = too_many
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    too_many.splice(position..position, insertion.bytes());
    assert_eq!(parse(&too_many), Err(ParseError::TooManyArguments));
}

#[test]
fn rejects_noncanonical_or_inconsistent_restart_limits() {
    assert_eq!(
        parse(&definition_with(("RestartLimit=3", "RestartLimit=03"))),
        Err(ParseError::InvalidInteger(Field::RestartLimit))
    );
    assert_eq!(
        parse(&definition_with(("RestartLimit=3", "RestartLimit=17"))),
        Err(ParseError::ValueOutOfRange(Field::RestartLimit))
    );
    assert_eq!(
        parse(&definition_with((
            "Restart=on-failure\nRestartLimit=3\nRestartBackoffYields=32",
            "Restart=never\nRestartLimit=3\nRestartBackoffYields=32",
        ))),
        Err(ParseError::InconsistentRestartPolicy)
    );
    assert_eq!(
        parse(&definition_with(("RestartLimit=3", "RestartLimit=0"))),
        Err(ParseError::InconsistentRestartPolicy)
    );
    assert_eq!(
        parse(&definition_with((
            "Restart=on-failure\nRestartLimit=3",
            "Restart=never\nRestartLimit=17",
        ))),
        Err(ParseError::ValueOutOfRange(Field::RestartLimit))
    );
    assert_eq!(
        parse(&definition_with((
            "RestartBackoffYields=32",
            "RestartBackoffYields=4294967296",
        ))),
        Err(ParseError::ValueOutOfRange(Field::RestartBackoffYields))
    );
    assert!(
        parse(&definition_with((
            "RestartBackoffYields=32",
            &format!("RestartBackoffYields={MAX_RESTART_BACKOFF_YIELDS}"),
        )))
        .is_ok()
    );
}

#[test]
fn enforces_uuid_name_and_path_canonical_boundaries() {
    assert_eq!(
        parse(&definition_with((
            "4c71a3aa-bc2c-4b38-8db4-737e0369ef8c",
            "4c71a3aa-bc2c-1b38-8db4-737e0369ef8c",
        ))),
        Err(ParseError::InvalidServiceId(
            ServiceIdTextError::InvalidVersion
        ))
    );
    assert_eq!(
        parse(&definition_with((
            "4c71a3aa-bc2c-4b38-8db4-737e0369ef8c",
            "4c71a3aa-bc2c-4b38-4db4-737e0369ef8c",
        ))),
        Err(ParseError::InvalidServiceId(
            ServiceIdTextError::InvalidVariant
        ))
    );
    for invalid in [
        "Name=.system",
        "Name=system.",
        "Name=system..probe",
        "Name=system.-probe",
        "Name=system.probe-",
    ] {
        assert_eq!(
            parse(&definition_with(("Name=system.definition-probe", invalid))),
            Err(ParseError::InvalidName)
        );
    }
    for invalid in [
        "Executable=System/bin/probe",
        "Executable=/System//probe",
        "Executable=/System/./probe",
        "Executable=/System/bin/probe/",
        "Executable=/System/bin/service probe",
    ] {
        assert_eq!(
            parse(&definition_with((
                "Executable=/System/bin/service-definition-probe",
                invalid,
            ))),
            Err(ParseError::InvalidExecutable)
        );
    }
}

#[test]
fn accepts_exact_file_argument_and_count_limits() {
    let mut maximum_file = definition_with(("not-present", "not-present"));
    let padding = MAX_DEFINITION_BYTES - maximum_file.len();
    assert!(padding >= 3);
    maximum_file.extend_from_slice(b"# ");
    maximum_file.extend(core::iter::repeat_n(b'x', padding - 3));
    maximum_file.push(b'\n');
    assert_eq!(maximum_file.len(), MAX_DEFINITION_BYTES);
    assert!(parse(&maximum_file).is_ok());

    let arguments = (0..MAX_ARGUMENTS)
        .map(|_| "Argument=value\n")
        .collect::<String>();
    let with_maximum_arguments = definition_with((
        "Readiness=notify\n",
        &format!("{arguments}Readiness=notify\n"),
    ));
    assert_eq!(
        parse(&with_maximum_arguments).unwrap().arguments().len(),
        MAX_ARGUMENTS
    );

    let maximum_argument = "é".repeat(MAX_ARGUMENT_BYTES / "é".len());
    assert_eq!(maximum_argument.len(), MAX_ARGUMENT_BYTES);
    let with_maximum_argument = definition_with((
        "Readiness=notify\n",
        &format!("Argument={maximum_argument}\nReadiness=notify\n"),
    ));
    assert!(parse(&with_maximum_argument).is_ok());

    let oversized_argument = "x".repeat(MAX_ARGUMENT_BYTES + 1);
    let with_oversized_argument = definition_with((
        "Readiness=notify\n",
        &format!("Argument={oversized_argument}\nReadiness=notify\n"),
    ));
    assert_eq!(
        parse(&with_oversized_argument),
        Err(ParseError::InvalidArgument)
    );
}

#[test]
fn rejects_invalid_utf8_and_never_panics_on_bounded_arbitrary_input() {
    let mut invalid_utf8 = definition_with(("Description=", "Description=\u{fffd}"));
    let replacement = invalid_utf8.iter().position(|byte| *byte == 0xef).unwrap();
    invalid_utf8[replacement] = 0xff;
    assert_eq!(parse(&invalid_utf8), Err(ParseError::InvalidUtf8));

    for seed in 0_u8..4 {
        for length in 0..=MAX_DEFINITION_BYTES {
            let bytes = (0..length)
                .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
                .collect::<Vec<_>>();
            assert!(std::panic::catch_unwind(|| parse(&bytes)).is_ok());
        }
    }
}
