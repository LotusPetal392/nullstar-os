use service_control::{
    CorrelationError, DecodeError, DesiredState, ListResponse, MutationResponseError,
    ObservedState, Operation, ProviderGeneration, RequestId, SERVICE_CONTROL_WIRE_BYTES,
    ServiceControlFailure, ServiceControlMessage, ServiceControlRequest, ServiceControlResponse,
    ServiceId, ServiceIdError, ServiceRecord, ServiceRecordError, TargetResponse,
};

const UUID: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];
const OTHER_UUID: [u8; 16] = [
    0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x47, 0x87, 0x98, 0xa9, 0xba, 0xcb, 0xdc, 0xed, 0xfe, 0x0f,
];

const LIST_REQUEST: [u8; SERVICE_CONTROL_WIRE_BYTES] = [
    0x4e, 0x53, 0x56, 0x43, 0x01, 0x00, 0x01, 0x01, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x44, 0x33, 0x22, 0x11, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const STATUS_REQUEST: [u8; SERVICE_CONTROL_WIRE_BYTES] = [
    0x4e, 0x53, 0x56, 0x43, 0x01, 0x00, 0x01, 0x02, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const LIST_RECORD_RESPONSE: [u8; SERVICE_CONTROL_WIRE_BYTES] = [
    0x4e, 0x53, 0x56, 0x43, 0x01, 0x00, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x05, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00,
    0x04, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const FAILED_RESTART_RESPONSE: [u8; SERVICE_CONTROL_WIRE_BYTES] = [
    0x4e, 0x53, 0x56, 0x43, 0x01, 0x00, 0x02, 0x05, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn request_id() -> RequestId {
    RequestId::new(0x0102_0304_0506_0708).unwrap()
}

fn service() -> ServiceId {
    ServiceId::from_bytes(UUID).unwrap()
}

fn other_service() -> ServiceId {
    ServiceId::from_bytes(OTHER_UUID).unwrap()
}

fn generation() -> ProviderGeneration {
    ProviderGeneration::new(0x1112_1314_1516_1718).unwrap()
}

fn ready_record() -> ServiceRecord {
    record_with_desired(DesiredState::Running)
}

fn record_with_desired(desired_state: DesiredState) -> ServiceRecord {
    ServiceRecord::new(
        service(),
        Some(generation()),
        ObservedState::Ready,
        desired_state,
    )
    .unwrap()
}

#[test]
fn representative_messages_match_literal_golden_bytes() {
    let list_request = ServiceControlMessage::request(
        request_id(),
        ServiceControlRequest::List {
            cursor: 0x1122_3344,
        },
    );
    let status_request = ServiceControlMessage::request(
        request_id(),
        ServiceControlRequest::Status { service: service() },
    );
    let list_response = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::list(ListResponse::record(5, ready_record(), 9).unwrap()),
    );
    let failed_restart = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::restart(TargetResponse::failure(
            service(),
            ServiceControlFailure::Busy,
        ))
        .unwrap(),
    );

    for (message, golden) in [
        (list_request, LIST_REQUEST),
        (status_request, STATUS_REQUEST),
        (list_response, LIST_RECORD_RESPONSE),
        (failed_restart, FAILED_RESTART_RESPONSE),
    ] {
        assert_eq!(message.encode(), golden);
        assert_eq!(ServiceControlMessage::decode(&golden), Ok(message));
    }
}

#[test]
fn every_operation_and_failure_round_trips() {
    let requests = [
        ServiceControlRequest::List { cursor: u32::MAX },
        ServiceControlRequest::Status { service: service() },
        ServiceControlRequest::Start { service: service() },
        ServiceControlRequest::Stop { service: service() },
        ServiceControlRequest::Restart { service: service() },
    ];
    for request in requests {
        let message = ServiceControlMessage::request(request_id(), request);
        assert_eq!(
            ServiceControlMessage::decode(&message.encode()),
            Ok(message)
        );
    }

    let target = TargetResponse::success(ready_record());
    let stopped_target = TargetResponse::success(record_with_desired(DesiredState::Stopped));
    let responses = [
        ServiceControlResponse::list(ListResponse::end(7)),
        ServiceControlResponse::status(target),
        ServiceControlResponse::start(target).unwrap(),
        ServiceControlResponse::stop(stopped_target).unwrap(),
        ServiceControlResponse::restart(target).unwrap(),
    ];
    for response in responses {
        let message = ServiceControlMessage::response(request_id(), response);
        assert_eq!(
            ServiceControlMessage::decode(&message.encode()),
            Ok(message)
        );
    }

    for (wire, failure) in [
        (1, ServiceControlFailure::NotFound),
        (2, ServiceControlFailure::AccessDenied),
        (3, ServiceControlFailure::InvalidState),
        (4, ServiceControlFailure::Busy),
        (5, ServiceControlFailure::Exhausted),
        (6, ServiceControlFailure::Unsupported),
    ] {
        let mut bytes = FAILED_RESTART_RESPONSE;
        bytes[50] = wire;
        assert_eq!(
            ServiceControlMessage::decode(&bytes),
            Ok(ServiceControlMessage::response(
                request_id(),
                ServiceControlResponse::restart(TargetResponse::failure(service(), failure))
                    .unwrap()
            ))
        );
    }
}

#[test]
fn framing_identity_and_closed_enums_are_strict() {
    assert_eq!(
        ServiceControlMessage::decode(&LIST_REQUEST[..63]),
        Err(DecodeError::InvalidLength)
    );
    let mut extended = LIST_REQUEST.to_vec();
    extended.push(0);
    assert_eq!(
        ServiceControlMessage::decode(&extended),
        Err(DecodeError::InvalidLength)
    );

    let mutations = [
        (0, 0, DecodeError::InvalidMagic),
        (4, 2, DecodeError::UnsupportedVersion),
        (5, 1, DecodeError::UnsupportedVersion),
        (6, 0, DecodeError::UnknownKind(0)),
        (6, 3, DecodeError::UnknownKind(3)),
        (7, 0, DecodeError::UnknownOperation(0)),
        (7, 6, DecodeError::UnknownOperation(6)),
    ];
    for (index, value, error) in mutations {
        let mut bytes = LIST_REQUEST;
        bytes[index] = value;
        assert_eq!(ServiceControlMessage::decode(&bytes), Err(error));
    }

    let mut bytes = LIST_REQUEST;
    bytes[8..16].fill(0);
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ZeroRequestId)
    );

    let mut bytes = LIST_RECORD_RESPONSE;
    bytes[48] = 11;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::UnknownObservedState(11))
    );
    let mut bytes = LIST_RECORD_RESPONSE;
    bytes[49] = 3;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::UnknownDesiredState(3))
    );
    let mut bytes = LIST_RECORD_RESPONSE;
    bytes[50] = 7;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::UnknownStatus(7))
    );

    let mut bytes = STATUS_REQUEST;
    bytes[22] = 0x16;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::InvalidServiceId(
            ServiceIdError::InvalidVersion
        ))
    );
    let mut bytes = STATUS_REQUEST;
    bytes[24] = 0x40;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::InvalidServiceId(
            ServiceIdError::InvalidVariant
        ))
    );

    let mut bytes = LIST_REQUEST;
    bytes[51] = 1;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::NonzeroFlags)
    );
    for index in 52..64 {
        let mut bytes = LIST_REQUEST;
        bytes[index] = 1;
        assert_eq!(
            ServiceControlMessage::decode(&bytes),
            Err(DecodeError::NonzeroReserved)
        );
    }
}

#[test]
fn request_relationship_invalid_matrix_is_rejected() {
    let mut bytes = LIST_REQUEST;
    bytes[16..32].copy_from_slice(&UUID);
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ServiceNotAllowed)
    );

    for (index, value, error) in [
        (32, 1, DecodeError::GenerationNotAllowed),
        (48, 1, DecodeError::ObservedStateNotAllowed),
        (49, 1, DecodeError::DesiredStateNotAllowed),
        (50, 1, DecodeError::StatusNotAllowed),
        (44, 1, DecodeError::NextCursorNotAllowed),
    ] {
        let mut bytes = LIST_REQUEST;
        bytes[index] = value;
        assert_eq!(ServiceControlMessage::decode(&bytes), Err(error));
    }

    let mut bytes = STATUS_REQUEST;
    bytes[16..32].fill(0);
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ServiceRequired)
    );
    let mut bytes = STATUS_REQUEST;
    bytes[40] = 1;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::CursorNotAllowed)
    );
}

#[test]
fn response_relationship_invalid_matrix_is_rejected() {
    let mut bytes = LIST_RECORD_RESPONSE;
    bytes[16..32].fill(0);
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::GenerationNotAllowed)
    );

    let mut bytes = LIST_RECORD_RESPONSE;
    bytes[44..48].copy_from_slice(&5_u32.to_le_bytes());
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::NextCursorNotAdvancing)
    );
    let mut bytes = LIST_RECORD_RESPONSE;
    bytes[44..48].copy_from_slice(&4_u32.to_le_bytes());
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::NextCursorNotAdvancing)
    );

    let mut failed_list = LIST_REQUEST;
    failed_list[6] = 2;
    failed_list[50] = ServiceControlFailure::Busy as u8;
    assert_eq!(
        ServiceControlMessage::decode(&failed_list),
        Ok(ServiceControlMessage::response(
            request_id(),
            ServiceControlResponse::list(ListResponse::failure(
                0x1122_3344,
                ServiceControlFailure::Busy
            ))
        ))
    );
    let mut bytes = failed_list;
    bytes[16..32].copy_from_slice(&UUID);
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ServiceNotAllowed)
    );
    let mut bytes = failed_list;
    bytes[44] = 1;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::NextCursorNotAllowed)
    );

    let mut bytes = FAILED_RESTART_RESPONSE;
    bytes[16..32].fill(0);
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ServiceRequired)
    );
    let mut bytes = FAILED_RESTART_RESPONSE;
    bytes[32] = 1;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::GenerationNotAllowed)
    );
    let mut bytes = FAILED_RESTART_RESPONSE;
    bytes[48] = ObservedState::Ready as u8;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ObservedStateNotAllowed)
    );
    let mut bytes = FAILED_RESTART_RESPONSE;
    bytes[49] = DesiredState::Running as u8;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::DesiredStateNotAllowed)
    );
    let mut bytes = FAILED_RESTART_RESPONSE;
    bytes[40] = 1;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::CursorNotAllowed)
    );
    let mut bytes = FAILED_RESTART_RESPONSE;
    bytes[44] = 1;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::NextCursorNotAllowed)
    );

    let mut successful_target = LIST_RECORD_RESPONSE;
    successful_target[7] = 2;
    successful_target[40..48].fill(0);
    let mut bytes = successful_target;
    bytes[48] = 0;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::ObservedStateRequired)
    );
    let mut bytes = successful_target;
    bytes[49] = 0;
    assert_eq!(
        ServiceControlMessage::decode(&bytes),
        Err(DecodeError::DesiredStateRequired)
    );
}

#[test]
fn state_generation_matrix_is_enforced_by_construction_and_decoding() {
    let states = [
        ObservedState::Defined,
        ObservedState::Activating,
        ObservedState::Starting,
        ObservedState::Ready,
        ObservedState::Degraded,
        ObservedState::Stopping,
        ObservedState::Terminating,
        ObservedState::Stopped,
        ObservedState::Failed,
        ObservedState::Quarantined,
    ];

    for state in states {
        let without_generation = ServiceRecord::new(service(), None, state, DesiredState::Running);
        let with_generation =
            ServiceRecord::new(service(), Some(generation()), state, DesiredState::Stopped);

        match state {
            ObservedState::Defined => {
                assert!(without_generation.is_ok());
                assert_eq!(
                    with_generation,
                    Err(ServiceRecordError::GenerationNotAllowed(state))
                );
            }
            ObservedState::Activating
            | ObservedState::Starting
            | ObservedState::Ready
            | ObservedState::Degraded
            | ObservedState::Stopping
            | ObservedState::Terminating
            | ObservedState::Failed => {
                assert_eq!(
                    without_generation,
                    Err(ServiceRecordError::GenerationRequired(state))
                );
                assert!(with_generation.is_ok());
            }
            ObservedState::Stopped | ObservedState::Quarantined => {
                assert!(without_generation.is_ok());
                assert!(with_generation.is_ok());
            }
        }

        for has_generation in [false, true] {
            let mut bytes = LIST_RECORD_RESPONSE;
            bytes[7] = 2;
            bytes[40..48].fill(0);
            bytes[48] = state as u8;
            bytes[49] = DesiredState::Running as u8;
            if !has_generation {
                bytes[32..40].fill(0);
            }

            let expected_error = match (state, has_generation) {
                (ObservedState::Defined, true) => Some(ServiceRecordError::GenerationNotAllowed(
                    ObservedState::Defined,
                )),
                (
                    ObservedState::Activating
                    | ObservedState::Starting
                    | ObservedState::Ready
                    | ObservedState::Degraded
                    | ObservedState::Stopping
                    | ObservedState::Terminating
                    | ObservedState::Failed,
                    false,
                ) => Some(ServiceRecordError::GenerationRequired(state)),
                _ => None,
            };
            if let Some(error) = expected_error {
                assert_eq!(
                    ServiceControlMessage::decode(&bytes),
                    Err(DecodeError::InvalidServiceRecord(error))
                );
            } else {
                let decoded = ServiceControlMessage::decode(&bytes).unwrap();
                assert_eq!(decoded.encode(), bytes);
            }
        }
    }
}

#[test]
fn list_response_constructor_rejects_impossible_cursors() {
    assert_eq!(
        ListResponse::record(5, ready_record(), 5),
        Err(service_control::ListResponseError::NextCursorNotAdvancing)
    );
    assert_eq!(
        ListResponse::record(5, ready_record(), 4),
        Err(service_control::ListResponseError::NextCursorNotAdvancing)
    );
    assert!(ListResponse::record(5, ready_record(), 0).is_ok());
    assert!(ListResponse::record(5, ready_record(), 6).is_ok());
}

#[test]
fn mutation_response_constructors_enforce_committed_desired_state() {
    let running = TargetResponse::success(record_with_desired(DesiredState::Running));
    let stopped = TargetResponse::success(record_with_desired(DesiredState::Stopped));

    assert_eq!(
        ServiceControlResponse::start(stopped),
        Err(MutationResponseError::DesiredStateMismatch {
            operation: Operation::Start,
            expected: DesiredState::Running,
            actual: DesiredState::Stopped,
        })
    );
    assert_eq!(
        ServiceControlResponse::stop(running),
        Err(MutationResponseError::DesiredStateMismatch {
            operation: Operation::Stop,
            expected: DesiredState::Stopped,
            actual: DesiredState::Running,
        })
    );
    assert_eq!(
        ServiceControlResponse::restart(stopped),
        Err(MutationResponseError::DesiredStateMismatch {
            operation: Operation::Restart,
            expected: DesiredState::Running,
            actual: DesiredState::Stopped,
        })
    );

    assert_eq!(
        ServiceControlResponse::start(running).unwrap().operation(),
        Operation::Start
    );
    assert_eq!(
        ServiceControlResponse::stop(stopped).unwrap().operation(),
        Operation::Stop
    );
    assert_eq!(
        ServiceControlResponse::restart(running)
            .unwrap()
            .operation(),
        Operation::Restart
    );
    assert_eq!(
        ServiceControlResponse::status(running).operation(),
        Operation::Status
    );
    assert_eq!(
        ServiceControlResponse::status(stopped).operation(),
        Operation::Status
    );

    let failure = TargetResponse::failure(service(), ServiceControlFailure::InvalidState);
    assert!(ServiceControlResponse::start(failure).is_ok());
    assert!(ServiceControlResponse::stop(failure).is_ok());
    assert!(ServiceControlResponse::restart(failure).is_ok());
}

#[test]
fn decoder_rejects_mutation_success_with_contradictory_desired_state() {
    for (operation_wire, desired_wire, operation, expected, actual) in [
        (
            3,
            DesiredState::Stopped as u8,
            Operation::Start,
            DesiredState::Running,
            DesiredState::Stopped,
        ),
        (
            4,
            DesiredState::Running as u8,
            Operation::Stop,
            DesiredState::Stopped,
            DesiredState::Running,
        ),
        (
            5,
            DesiredState::Stopped as u8,
            Operation::Restart,
            DesiredState::Running,
            DesiredState::Stopped,
        ),
    ] {
        let mut bytes = LIST_RECORD_RESPONSE;
        bytes[7] = operation_wire;
        bytes[40..48].fill(0);
        bytes[49] = desired_wire;
        assert_eq!(
            ServiceControlMessage::decode(&bytes),
            Err(DecodeError::MutationDesiredStateMismatch {
                operation,
                expected,
                actual,
            })
        );
    }

    for (operation_wire, desired_wire, operation) in [
        (2, DesiredState::Stopped as u8, Operation::Status),
        (2, DesiredState::Running as u8, Operation::Status),
        (3, DesiredState::Running as u8, Operation::Start),
        (4, DesiredState::Stopped as u8, Operation::Stop),
        (5, DesiredState::Running as u8, Operation::Restart),
    ] {
        let mut bytes = LIST_RECORD_RESPONSE;
        bytes[7] = operation_wire;
        bytes[40..48].fill(0);
        bytes[49] = desired_wire;
        let decoded = ServiceControlMessage::decode(&bytes).unwrap();
        assert_eq!(decoded.operation(), operation);
        assert_eq!(decoded.encode(), bytes);
    }
}

#[test]
fn response_correlation_checks_every_echo_field() {
    let list_request =
        ServiceControlMessage::request(request_id(), ServiceControlRequest::List { cursor: 5 });
    let list_response = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::list(ListResponse::record(5, ready_record(), 9).unwrap()),
    );
    assert_eq!(list_response.validate_response_to(&list_request), Ok(()));

    let target_request = ServiceControlMessage::request(
        request_id(),
        ServiceControlRequest::Restart { service: service() },
    );
    let target_response = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::restart(TargetResponse::failure(
            service(),
            ServiceControlFailure::Busy,
        ))
        .unwrap(),
    );
    assert_eq!(
        target_response.validate_response_to(&target_request),
        Ok(())
    );

    assert_eq!(
        list_request.validate_response_to(&list_request),
        Err(CorrelationError::ResponseExpected)
    );
    assert_eq!(
        list_response.validate_response_to(&target_response),
        Err(CorrelationError::RequestExpected)
    );

    let wrong_id = ServiceControlMessage::response(
        RequestId::new(9).unwrap(),
        ServiceControlResponse::list(ListResponse::end(5)),
    );
    assert_eq!(
        wrong_id.validate_response_to(&list_request),
        Err(CorrelationError::RequestIdMismatch)
    );

    let wrong_operation = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::status(TargetResponse::failure(
            service(),
            ServiceControlFailure::NotFound,
        )),
    );
    assert_eq!(
        wrong_operation.validate_response_to(&target_request),
        Err(CorrelationError::OperationMismatch)
    );

    let wrong_service = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::restart(TargetResponse::failure(
            other_service(),
            ServiceControlFailure::NotFound,
        ))
        .unwrap(),
    );
    assert_eq!(
        wrong_service.validate_response_to(&target_request),
        Err(CorrelationError::TargetServiceMismatch)
    );

    let wrong_cursor = ServiceControlMessage::response(
        request_id(),
        ServiceControlResponse::list(ListResponse::end(6)),
    );
    assert_eq!(
        wrong_cursor.validate_response_to(&list_request),
        Err(CorrelationError::ListCursorMismatch)
    );
}
