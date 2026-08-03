use service_route::{ProviderGeneration, ServiceId, ServiceIdError};

use crate::{
    DesiredState, ListOutcome, ListResponse, MutationResponseError, ObservedState, Operation,
    RequestId, ServiceControlFailure, ServiceControlMessage, ServiceControlRequest,
    ServiceControlResponse, ServiceRecord, ServiceRecordError, TargetOutcome, TargetResponse,
    types::ResponseKind,
};

pub const SERVICE_CONTROL_MAGIC: [u8; 4] = *b"NSVC";
pub const SERVICE_CONTROL_VERSION: u16 = 1;
pub const SERVICE_CONTROL_WIRE_BYTES: usize = 64;

const KIND_REQUEST: u8 = 1;
const KIND_RESPONSE: u8 = 2;
const STATUS_SUCCESS: u8 = 0;

/// Failure to decode one exact canonical `NSVC` v1 message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidLength,
    InvalidMagic,
    UnsupportedVersion,
    UnknownKind(u8),
    UnknownOperation(u8),
    ZeroRequestId,
    InvalidServiceId(ServiceIdError),
    UnknownObservedState(u8),
    UnknownDesiredState(u8),
    UnknownStatus(u8),
    NonzeroFlags,
    NonzeroReserved,
    ServiceNotAllowed,
    ServiceRequired,
    StatusNotAllowed,
    GenerationNotAllowed,
    ObservedStateNotAllowed,
    ObservedStateRequired,
    DesiredStateNotAllowed,
    DesiredStateRequired,
    CursorNotAllowed,
    NextCursorNotAllowed,
    NextCursorNotAdvancing,
    InvalidServiceRecord(ServiceRecordError),
    MutationDesiredStateMismatch {
        operation: Operation,
        expected: DesiredState,
        actual: DesiredState,
    },
}

#[derive(Clone, Copy)]
struct DecodedFields {
    request_id: RequestId,
    operation: Operation,
    service: Option<ServiceId>,
    generation: Option<ProviderGeneration>,
    cursor: u32,
    next_cursor: u32,
    observed_state: Option<ObservedState>,
    desired_state: Option<DesiredState>,
    failure: Option<ServiceControlFailure>,
}

impl ServiceControlMessage {
    /// Encodes this message into the exact 64-byte `NSVC` v1 representation.
    ///
    /// Integer fields use little-endian order. Service UUID bytes retain RFC/network order.
    pub fn encode(self) -> [u8; SERVICE_CONTROL_WIRE_BYTES] {
        let mut output = [0; SERVICE_CONTROL_WIRE_BYTES];
        output[0..4].copy_from_slice(&SERVICE_CONTROL_MAGIC);
        output[4..6].copy_from_slice(&SERVICE_CONTROL_VERSION.to_le_bytes());
        output[8..16].copy_from_slice(&self.request_id().get().to_le_bytes());
        output[7] = self.operation() as u8;

        match self {
            Self::Request { request, .. } => {
                output[6] = KIND_REQUEST;
                encode_request(request, &mut output);
            }
            Self::Response { response, .. } => {
                output[6] = KIND_RESPONSE;
                encode_response(response, &mut output);
            }
        }
        output
    }

    /// Decodes one exact canonical 64-byte `NSVC` v1 message.
    pub fn decode(input: &[u8]) -> Result<Self, DecodeError> {
        if input.len() != SERVICE_CONTROL_WIRE_BYTES {
            return Err(DecodeError::InvalidLength);
        }
        if input[0..4] != SERVICE_CONTROL_MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        if read_u16(input, 4) != SERVICE_CONTROL_VERSION {
            return Err(DecodeError::UnsupportedVersion);
        }

        let kind = input[6];
        if !matches!(kind, KIND_REQUEST | KIND_RESPONSE) {
            return Err(DecodeError::UnknownKind(kind));
        }
        let operation =
            Operation::from_wire(input[7]).ok_or(DecodeError::UnknownOperation(input[7]))?;
        let request_id = RequestId::new(read_u64(input, 8)).ok_or(DecodeError::ZeroRequestId)?;

        if input[51] != 0 {
            return Err(DecodeError::NonzeroFlags);
        }
        if input[52..64].iter().any(|byte| *byte != 0) {
            return Err(DecodeError::NonzeroReserved);
        }

        let fields = DecodedFields {
            request_id,
            operation,
            service: decode_service(input)?,
            generation: ProviderGeneration::new(read_u64(input, 32)),
            cursor: read_u32(input, 40),
            next_cursor: read_u32(input, 44),
            observed_state: decode_observed_state(input[48])?,
            desired_state: decode_desired_state(input[49])?,
            failure: decode_status(input[50])?,
        };

        if kind == KIND_REQUEST {
            decode_request(fields)
        } else {
            decode_response(fields)
        }
    }
}

fn encode_request(request: ServiceControlRequest, output: &mut [u8; SERVICE_CONTROL_WIRE_BYTES]) {
    match request {
        ServiceControlRequest::List { cursor } => write_u32(output, 40, cursor),
        ServiceControlRequest::Status { service }
        | ServiceControlRequest::Start { service }
        | ServiceControlRequest::Stop { service }
        | ServiceControlRequest::Restart { service } => write_service(output, service),
    }
}

fn encode_response(
    response: ServiceControlResponse,
    output: &mut [u8; SERVICE_CONTROL_WIRE_BYTES],
) {
    match response.kind() {
        ResponseKind::List(response) => {
            write_u32(output, 40, response.cursor());
            match response.outcome() {
                ListOutcome::End => {}
                ListOutcome::Record {
                    record,
                    next_cursor,
                } => {
                    write_record(output, record);
                    write_u32(output, 44, next_cursor);
                }
                ListOutcome::Failure(failure) => output[50] = failure as u8,
            }
        }
        ResponseKind::Status(response)
        | ResponseKind::Start(response)
        | ResponseKind::Stop(response)
        | ResponseKind::Restart(response) => match response.outcome() {
            TargetOutcome::Record(record) => write_record(output, record),
            TargetOutcome::Failure(failure) => {
                write_service(output, response.service());
                output[50] = failure as u8;
            }
        },
    }
}

fn write_record(output: &mut [u8; SERVICE_CONTROL_WIRE_BYTES], record: ServiceRecord) {
    write_service(output, record.service());
    if let Some(generation) = record.generation() {
        output[32..40].copy_from_slice(&generation.get().to_le_bytes());
    }
    output[48] = record.observed_state() as u8;
    output[49] = record.desired_state() as u8;
}

fn write_service(output: &mut [u8; SERVICE_CONTROL_WIRE_BYTES], service: ServiceId) {
    output[16..32].copy_from_slice(service.as_bytes());
}

fn write_u32(output: &mut [u8; SERVICE_CONTROL_WIRE_BYTES], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn decode_request(fields: DecodedFields) -> Result<ServiceControlMessage, DecodeError> {
    if fields.failure.is_some() {
        return Err(DecodeError::StatusNotAllowed);
    }
    reject_record_payload(fields)?;
    if fields.next_cursor != 0 {
        return Err(DecodeError::NextCursorNotAllowed);
    }

    let request = match fields.operation {
        Operation::List => {
            if fields.service.is_some() {
                return Err(DecodeError::ServiceNotAllowed);
            }
            ServiceControlRequest::List {
                cursor: fields.cursor,
            }
        }
        Operation::Status | Operation::Start | Operation::Stop | Operation::Restart => {
            if fields.cursor != 0 {
                return Err(DecodeError::CursorNotAllowed);
            }
            let service = fields.service.ok_or(DecodeError::ServiceRequired)?;
            match fields.operation {
                Operation::Status => ServiceControlRequest::Status { service },
                Operation::Start => ServiceControlRequest::Start { service },
                Operation::Stop => ServiceControlRequest::Stop { service },
                Operation::Restart => ServiceControlRequest::Restart { service },
                Operation::List => unreachable!(),
            }
        }
    };
    Ok(ServiceControlMessage::request(fields.request_id, request))
}

fn decode_response(fields: DecodedFields) -> Result<ServiceControlMessage, DecodeError> {
    let response = match fields.operation {
        Operation::List => ServiceControlResponse::list(decode_list_response(fields)?),
        Operation::Status | Operation::Start | Operation::Stop | Operation::Restart => {
            let target = decode_target_response(fields)?;
            match fields.operation {
                Operation::Status => ServiceControlResponse::status(target),
                Operation::Start => {
                    ServiceControlResponse::start(target).map_err(map_mutation_error)?
                }
                Operation::Stop => {
                    ServiceControlResponse::stop(target).map_err(map_mutation_error)?
                }
                Operation::Restart => {
                    ServiceControlResponse::restart(target).map_err(map_mutation_error)?
                }
                Operation::List => unreachable!(),
            }
        }
    };
    Ok(ServiceControlMessage::response(fields.request_id, response))
}

fn map_mutation_error(error: MutationResponseError) -> DecodeError {
    match error {
        MutationResponseError::DesiredStateMismatch {
            operation,
            expected,
            actual,
        } => DecodeError::MutationDesiredStateMismatch {
            operation,
            expected,
            actual,
        },
    }
}

fn decode_list_response(fields: DecodedFields) -> Result<ListResponse, DecodeError> {
    if let Some(failure) = fields.failure {
        if fields.service.is_some() {
            return Err(DecodeError::ServiceNotAllowed);
        }
        reject_record_payload(fields)?;
        if fields.next_cursor != 0 {
            return Err(DecodeError::NextCursorNotAllowed);
        }
        return Ok(ListResponse::failure(fields.cursor, failure));
    }

    let Some(service) = fields.service else {
        reject_record_payload(fields)?;
        if fields.next_cursor != 0 {
            return Err(DecodeError::NextCursorNotAllowed);
        }
        return Ok(ListResponse::end(fields.cursor));
    };

    let record = decode_record(fields, service)?;
    if fields.next_cursor != 0 && fields.next_cursor <= fields.cursor {
        return Err(DecodeError::NextCursorNotAdvancing);
    }
    ListResponse::record(fields.cursor, record, fields.next_cursor)
        .map_err(|_| DecodeError::NextCursorNotAdvancing)
}

fn decode_target_response(fields: DecodedFields) -> Result<TargetResponse, DecodeError> {
    if fields.cursor != 0 {
        return Err(DecodeError::CursorNotAllowed);
    }
    if fields.next_cursor != 0 {
        return Err(DecodeError::NextCursorNotAllowed);
    }
    let service = fields.service.ok_or(DecodeError::ServiceRequired)?;

    if let Some(failure) = fields.failure {
        reject_record_payload(fields)?;
        Ok(TargetResponse::failure(service, failure))
    } else {
        Ok(TargetResponse::success(decode_record(fields, service)?))
    }
}

fn decode_record(fields: DecodedFields, service: ServiceId) -> Result<ServiceRecord, DecodeError> {
    let observed_state = fields
        .observed_state
        .ok_or(DecodeError::ObservedStateRequired)?;
    let desired_state = fields
        .desired_state
        .ok_or(DecodeError::DesiredStateRequired)?;
    ServiceRecord::new(service, fields.generation, observed_state, desired_state)
        .map_err(DecodeError::InvalidServiceRecord)
}

fn reject_record_payload(fields: DecodedFields) -> Result<(), DecodeError> {
    if fields.generation.is_some() {
        return Err(DecodeError::GenerationNotAllowed);
    }
    if fields.observed_state.is_some() {
        return Err(DecodeError::ObservedStateNotAllowed);
    }
    if fields.desired_state.is_some() {
        return Err(DecodeError::DesiredStateNotAllowed);
    }
    Ok(())
}

fn decode_service(input: &[u8]) -> Result<Option<ServiceId>, DecodeError> {
    if input[16..32].iter().all(|byte| *byte == 0) {
        return Ok(None);
    }
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&input[16..32]);
    ServiceId::from_bytes(bytes)
        .map(Some)
        .map_err(DecodeError::InvalidServiceId)
}

fn decode_observed_state(value: u8) -> Result<Option<ObservedState>, DecodeError> {
    if value == 0 {
        Ok(None)
    } else {
        ObservedState::from_wire(value)
            .map(Some)
            .ok_or(DecodeError::UnknownObservedState(value))
    }
}

fn decode_desired_state(value: u8) -> Result<Option<DesiredState>, DecodeError> {
    if value == 0 {
        Ok(None)
    } else {
        DesiredState::from_wire(value)
            .map(Some)
            .ok_or(DecodeError::UnknownDesiredState(value))
    }
}

fn decode_status(value: u8) -> Result<Option<ServiceControlFailure>, DecodeError> {
    if value == STATUS_SUCCESS {
        Ok(None)
    } else {
        ServiceControlFailure::from_wire(value)
            .map(Some)
            .ok_or(DecodeError::UnknownStatus(value))
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
        input[offset + 4],
        input[offset + 5],
        input[offset + 6],
        input[offset + 7],
    ])
}
