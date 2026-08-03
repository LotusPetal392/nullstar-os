use core::num::NonZeroU64;

use service_route::{ProviderGeneration, ServiceId};

/// A nonzero identifier shared by one request and its response.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(NonZeroU64);

impl RequestId {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Service-control operation carried by an `NSVC` message.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    List = 1,
    Status = 2,
    Start = 3,
    Stop = 4,
    Restart = 5,
}

impl Operation {
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::List),
            2 => Some(Self::Status),
            3 => Some(Self::Start),
            4 => Some(Self::Stop),
            5 => Some(Self::Restart),
            _ => None,
        }
    }
}

/// Observed service lifecycle state.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedState {
    Defined = 1,
    Activating = 2,
    Starting = 3,
    Ready = 4,
    Degraded = 5,
    Stopping = 6,
    Terminating = 7,
    Stopped = 8,
    Failed = 9,
    Quarantined = 10,
}

impl ObservedState {
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Defined),
            2 => Some(Self::Activating),
            3 => Some(Self::Starting),
            4 => Some(Self::Ready),
            5 => Some(Self::Degraded),
            6 => Some(Self::Stopping),
            7 => Some(Self::Terminating),
            8 => Some(Self::Stopped),
            9 => Some(Self::Failed),
            10 => Some(Self::Quarantined),
            _ => None,
        }
    }

    const fn generation_rule(self) -> GenerationRule {
        match self {
            Self::Defined => GenerationRule::Forbidden,
            Self::Activating
            | Self::Starting
            | Self::Ready
            | Self::Degraded
            | Self::Stopping
            | Self::Terminating
            | Self::Failed => GenerationRule::Required,
            Self::Stopped | Self::Quarantined => GenerationRule::Optional,
        }
    }
}

#[derive(Clone, Copy)]
enum GenerationRule {
    Forbidden,
    Required,
    Optional,
}

/// Desired service lifecycle state.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesiredState {
    Stopped = 1,
    Running = 2,
}

impl DesiredState {
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Stopped),
            2 => Some(Self::Running),
            _ => None,
        }
    }
}

/// Failure status returned by a service-control operation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceControlFailure {
    NotFound = 1,
    AccessDenied = 2,
    InvalidState = 3,
    Busy = 4,
    Exhausted = 5,
    Unsupported = 6,
}

impl ServiceControlFailure {
    pub(crate) const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::NotFound),
            2 => Some(Self::AccessDenied),
            3 => Some(Self::InvalidState),
            4 => Some(Self::Busy),
            5 => Some(Self::Exhausted),
            6 => Some(Self::Unsupported),
            _ => None,
        }
    }
}

/// A service record whose generation is consistent with its observed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceRecord {
    service: ServiceId,
    generation: Option<ProviderGeneration>,
    observed_state: ObservedState,
    desired_state: DesiredState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceRecordError {
    GenerationRequired(ObservedState),
    GenerationNotAllowed(ObservedState),
}

impl ServiceRecord {
    pub const fn new(
        service: ServiceId,
        generation: Option<ProviderGeneration>,
        observed_state: ObservedState,
        desired_state: DesiredState,
    ) -> Result<Self, ServiceRecordError> {
        match (observed_state.generation_rule(), generation) {
            (GenerationRule::Forbidden, Some(_)) => {
                return Err(ServiceRecordError::GenerationNotAllowed(observed_state));
            }
            (GenerationRule::Required, None) => {
                return Err(ServiceRecordError::GenerationRequired(observed_state));
            }
            _ => {}
        }
        Ok(Self {
            service,
            generation,
            observed_state,
            desired_state,
        })
    }

    pub const fn service(self) -> ServiceId {
        self.service
    }

    pub const fn generation(self) -> Option<ProviderGeneration> {
        self.generation
    }

    pub const fn observed_state(self) -> ObservedState {
        self.observed_state
    }

    pub const fn desired_state(self) -> DesiredState {
        self.desired_state
    }
}

/// Valid result carried by a list response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListOutcome {
    End,
    Record {
        record: ServiceRecord,
        next_cursor: u32,
    },
    Failure(ServiceControlFailure),
}

/// A list response with its echoed cursor and a canonical result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListResponse {
    cursor: u32,
    outcome: ListOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListResponseError {
    NextCursorNotAdvancing,
}

impl ListResponse {
    pub const fn end(cursor: u32) -> Self {
        Self {
            cursor,
            outcome: ListOutcome::End,
        }
    }

    pub const fn record(
        cursor: u32,
        record: ServiceRecord,
        next_cursor: u32,
    ) -> Result<Self, ListResponseError> {
        if next_cursor != 0 && next_cursor <= cursor {
            return Err(ListResponseError::NextCursorNotAdvancing);
        }
        Ok(Self {
            cursor,
            outcome: ListOutcome::Record {
                record,
                next_cursor,
            },
        })
    }

    pub const fn failure(cursor: u32, failure: ServiceControlFailure) -> Self {
        Self {
            cursor,
            outcome: ListOutcome::Failure(failure),
        }
    }

    pub const fn cursor(self) -> u32 {
        self.cursor
    }

    pub const fn outcome(self) -> ListOutcome {
        self.outcome
    }
}

/// Valid result carried by a target-operation response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetOutcome {
    Record(ServiceRecord),
    Failure(ServiceControlFailure),
}

/// A target-operation response that always identifies the target service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetResponse {
    service: ServiceId,
    outcome: TargetOutcome,
}

impl TargetResponse {
    pub const fn success(record: ServiceRecord) -> Self {
        Self {
            service: record.service(),
            outcome: TargetOutcome::Record(record),
        }
    }

    pub const fn failure(service: ServiceId, failure: ServiceControlFailure) -> Self {
        Self {
            service,
            outcome: TargetOutcome::Failure(failure),
        }
    }

    pub const fn service(self) -> ServiceId {
        self.service
    }

    pub const fn outcome(self) -> TargetOutcome {
        self.outcome
    }
}

/// Semantic service-control request payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceControlRequest {
    List { cursor: u32 },
    Status { service: ServiceId },
    Start { service: ServiceId },
    Stop { service: ServiceId },
    Restart { service: ServiceId },
}

impl ServiceControlRequest {
    pub const fn operation(self) -> Operation {
        match self {
            Self::List { .. } => Operation::List,
            Self::Status { .. } => Operation::Status,
            Self::Start { .. } => Operation::Start,
            Self::Stop { .. } => Operation::Stop,
            Self::Restart { .. } => Operation::Restart,
        }
    }

    pub const fn service(self) -> Option<ServiceId> {
        match self {
            Self::List { .. } => None,
            Self::Status { service }
            | Self::Start { service }
            | Self::Stop { service }
            | Self::Restart { service } => Some(service),
        }
    }

    pub const fn cursor(self) -> u32 {
        match self {
            Self::List { cursor } => cursor,
            Self::Status { .. } | Self::Start { .. } | Self::Stop { .. } | Self::Restart { .. } => {
                0
            }
        }
    }
}

/// Semantic service-control response payload.
///
/// The internal operation kind is private so mutation successes can only be created through the
/// checked [`Self::start`], [`Self::stop`], and [`Self::restart`] constructors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceControlResponse {
    kind: ResponseKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseKind {
    List(ListResponse),
    Status(TargetResponse),
    Start(TargetResponse),
    Stop(TargetResponse),
    Restart(TargetResponse),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationResponseError {
    DesiredStateMismatch {
        operation: Operation,
        expected: DesiredState,
        actual: DesiredState,
    },
}

impl ServiceControlResponse {
    pub const fn list(response: ListResponse) -> Self {
        Self {
            kind: ResponseKind::List(response),
        }
    }

    pub const fn status(response: TargetResponse) -> Self {
        Self {
            kind: ResponseKind::Status(response),
        }
    }

    pub const fn start(response: TargetResponse) -> Result<Self, MutationResponseError> {
        Self::mutation(Operation::Start, DesiredState::Running, response)
    }

    pub const fn stop(response: TargetResponse) -> Result<Self, MutationResponseError> {
        Self::mutation(Operation::Stop, DesiredState::Stopped, response)
    }

    pub const fn restart(response: TargetResponse) -> Result<Self, MutationResponseError> {
        Self::mutation(Operation::Restart, DesiredState::Running, response)
    }

    const fn mutation(
        operation: Operation,
        expected: DesiredState,
        response: TargetResponse,
    ) -> Result<Self, MutationResponseError> {
        if let TargetOutcome::Record(record) = response.outcome()
            && record.desired_state() as u8 != expected as u8
        {
            return Err(MutationResponseError::DesiredStateMismatch {
                operation,
                expected,
                actual: record.desired_state(),
            });
        }
        let kind = match operation {
            Operation::Start => ResponseKind::Start(response),
            Operation::Stop => ResponseKind::Stop(response),
            Operation::Restart => ResponseKind::Restart(response),
            Operation::List | Operation::Status => unreachable!(),
        };
        Ok(Self { kind })
    }

    pub const fn operation(self) -> Operation {
        match self.kind {
            ResponseKind::List(_) => Operation::List,
            ResponseKind::Status(_) => Operation::Status,
            ResponseKind::Start(_) => Operation::Start,
            ResponseKind::Stop(_) => Operation::Stop,
            ResponseKind::Restart(_) => Operation::Restart,
        }
    }

    pub const fn service(self) -> Option<ServiceId> {
        match self.kind {
            ResponseKind::List(response) => match response.outcome() {
                ListOutcome::Record { record, .. } => Some(record.service()),
                ListOutcome::End | ListOutcome::Failure(_) => None,
            },
            ResponseKind::Status(response)
            | ResponseKind::Start(response)
            | ResponseKind::Stop(response)
            | ResponseKind::Restart(response) => Some(response.service()),
        }
    }

    pub const fn cursor(self) -> u32 {
        match self.kind {
            ResponseKind::List(response) => response.cursor(),
            ResponseKind::Status(_)
            | ResponseKind::Start(_)
            | ResponseKind::Stop(_)
            | ResponseKind::Restart(_) => 0,
        }
    }

    pub const fn list_response(self) -> Option<ListResponse> {
        match self.kind {
            ResponseKind::List(response) => Some(response),
            ResponseKind::Status(_)
            | ResponseKind::Start(_)
            | ResponseKind::Stop(_)
            | ResponseKind::Restart(_) => None,
        }
    }

    pub const fn target_response(self) -> Option<TargetResponse> {
        match self.kind {
            ResponseKind::List(_) => None,
            ResponseKind::Status(response)
            | ResponseKind::Start(response)
            | ResponseKind::Stop(response)
            | ResponseKind::Restart(response) => Some(response),
        }
    }

    pub(crate) const fn kind(self) -> ResponseKind {
        self.kind
    }
}

/// One canonical request or response, paired with a nonzero request ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceControlMessage {
    Request {
        request_id: RequestId,
        request: ServiceControlRequest,
    },
    Response {
        request_id: RequestId,
        response: ServiceControlResponse,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrelationError {
    ResponseExpected,
    RequestExpected,
    RequestIdMismatch,
    OperationMismatch,
    TargetServiceMismatch,
    ListCursorMismatch,
}

impl ServiceControlMessage {
    pub const fn request(request_id: RequestId, request: ServiceControlRequest) -> Self {
        Self::Request {
            request_id,
            request,
        }
    }

    pub const fn response(request_id: RequestId, response: ServiceControlResponse) -> Self {
        Self::Response {
            request_id,
            response,
        }
    }

    pub const fn request_id(self) -> RequestId {
        match self {
            Self::Request { request_id, .. } | Self::Response { request_id, .. } => request_id,
        }
    }

    pub const fn operation(self) -> Operation {
        match self {
            Self::Request { request, .. } => request.operation(),
            Self::Response { response, .. } => response.operation(),
        }
    }

    /// Verifies that this message is the response corresponding to `request`.
    pub fn validate_response_to(&self, request: &Self) -> Result<(), CorrelationError> {
        let (response_id, response) = match *self {
            Self::Response {
                request_id,
                response,
            } => (request_id, response),
            Self::Request { .. } => return Err(CorrelationError::ResponseExpected),
        };
        let (request_id, request) = match *request {
            Self::Request {
                request_id,
                request,
            } => (request_id, request),
            Self::Response { .. } => return Err(CorrelationError::RequestExpected),
        };

        if response_id != request_id {
            return Err(CorrelationError::RequestIdMismatch);
        }
        if response.operation() != request.operation() {
            return Err(CorrelationError::OperationMismatch);
        }
        match request {
            ServiceControlRequest::List { cursor } => {
                if response.cursor() != cursor {
                    return Err(CorrelationError::ListCursorMismatch);
                }
            }
            ServiceControlRequest::Status { service }
            | ServiceControlRequest::Start { service }
            | ServiceControlRequest::Stop { service }
            | ServiceControlRequest::Restart { service } => {
                if response.service() != Some(service) {
                    return Err(CorrelationError::TargetServiceMismatch);
                }
            }
        }
        Ok(())
    }
}
