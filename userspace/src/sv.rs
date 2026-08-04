//! Allocation-free, bounded client operations for read-only service observation.

use service_control::{
    DesiredState, ListOutcome, ObservedState, RequestId, ServiceControlFailure,
    ServiceControlRequest, ServiceControlResponse, ServiceId, ServiceRecord, TargetOutcome,
};

pub use crate::service_control::{
    LOGGING_SERVICE_ID, NULLFS_SERVICE_ID, TMPFS_SERVICE_ID, VFS_SERVICE_ID, service_id,
    service_name,
};
use crate::{
    abi::INIT_PROCESS_ID,
    ipc::CapabilityHandle,
    service_control::{BeginError, CompleteError, ControlExchange},
    syscall::{self, STDOUT},
};

/// Maximum number of one-record pages accepted by one `list` invocation.
pub const LIST_PAGE_BUDGET: u32 = 64;
/// Maximum number of requests sent by one public operation.
pub const REQUEST_BUDGET: u32 = 64;
/// Maximum number of cooperative yields spent waiting for all responses in one operation.
pub const YIELD_BUDGET: u32 = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    PageBudgetExhausted,
    RequestBudgetExhausted,
    YieldBudgetExhausted,
    RequestIdExhausted,
    Begin(BeginError),
    Complete(CompleteError),
    Yield(syscall::Errno),
    UnexpectedServer { expected: u64, actual: u64 },
    UnexpectedResponse,
    ServiceFailure(ServiceControlFailure),
    Output(syscall::Errno),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Budgets {
    pages: u32,
    requests: u32,
    yields: u32,
}

impl Budgets {
    const fn new() -> Self {
        Self {
            pages: LIST_PAGE_BUDGET,
            requests: REQUEST_BUDGET,
            yields: YIELD_BUDGET,
        }
    }

    fn spend_page(&mut self) -> Result<(), Error> {
        if self.pages == 0 {
            return Err(Error::PageBudgetExhausted);
        }
        self.pages -= 1;
        Ok(())
    }

    fn spend_request(&mut self) -> Result<(), Error> {
        if self.requests == 0 {
            return Err(Error::RequestBudgetExhausted);
        }
        self.requests -= 1;
        Ok(())
    }

    fn yield_once(&mut self) -> Result<(), Error> {
        if self.yields == 0 {
            return Err(Error::YieldBudgetExhausted);
        }
        self.yields -= 1;
        syscall::yield_now().map_err(Error::Yield)
    }
}

struct RequestIds {
    next: u64,
}

impl RequestIds {
    const fn new() -> Self {
        Self { next: 1 }
    }

    fn take(&mut self) -> Result<RequestId, Error> {
        let request_id = RequestId::new(self.next).ok_or(Error::RequestIdExhausted)?;
        self.next = self.next.checked_add(1).ok_or(Error::RequestIdExhausted)?;
        Ok(request_id)
    }
}

/// Lists visible services over a borrowed exact-`SEND` observation grant.
///
/// Records are printed as they arrive; no list is allocated or retained. The grant remains
/// caller-owned on every return path.
pub fn list(observation_grant: CapabilityHandle) -> Result<(), Error> {
    let mut budgets = Budgets::new();
    let mut request_ids = RequestIds::new();
    let mut cursor = 0;

    loop {
        budgets.spend_page()?;
        let response = request(
            observation_grant,
            ServiceControlRequest::List { cursor },
            &mut request_ids,
            &mut budgets,
        )?;
        let list = response.list_response().ok_or(Error::UnexpectedResponse)?;
        match list.outcome() {
            ListOutcome::End => return Ok(()),
            ListOutcome::Record {
                record,
                next_cursor,
            } => {
                print_record(record)?;
                if next_cursor == 0 {
                    return Ok(());
                }
                cursor = next_cursor;
            }
            ListOutcome::Failure(failure) => return Err(Error::ServiceFailure(failure)),
        }
    }
}

/// Queries and prints one service over a borrowed exact-`SEND` observation grant.
pub fn status(observation_grant: CapabilityHandle, service: ServiceId) -> Result<(), Error> {
    let mut budgets = Budgets::new();
    let mut request_ids = RequestIds::new();
    let response = request(
        observation_grant,
        ServiceControlRequest::Status { service },
        &mut request_ids,
        &mut budgets,
    )?;
    let target = response
        .target_response()
        .ok_or(Error::UnexpectedResponse)?;
    match target.outcome() {
        TargetOutcome::Record(record) => print_record(record),
        TargetOutcome::Failure(failure) => Err(Error::ServiceFailure(failure)),
    }
}

fn request(
    observation_grant: CapabilityHandle,
    request: ServiceControlRequest,
    request_ids: &mut RequestIds,
    budgets: &mut Budgets,
) -> Result<ServiceControlResponse, Error> {
    budgets.spend_request()?;
    let request_id = request_ids.take()?;
    let mut exchange =
        ControlExchange::begin(observation_grant, request_id, request).map_err(Error::Begin)?;
    loop {
        match exchange.try_complete() {
            Ok(Some(reply)) => {
                if reply.server_process_id() != INIT_PROCESS_ID {
                    return Err(Error::UnexpectedServer {
                        expected: INIT_PROCESS_ID,
                        actual: reply.server_process_id(),
                    });
                }
                return Ok(reply.response());
            }
            Ok(None) => budgets.yield_once()?,
            Err(error) => return Err(Error::Complete(error)),
        }
    }
}

fn print_record(record: ServiceRecord) -> Result<(), Error> {
    write_service(record.service())?;
    write_bytes(b" ")?;
    write_bytes(observed_name(record.observed_state()))?;
    write_bytes(b" desired=")?;
    write_bytes(desired_name(record.desired_state()))?;
    if let Some(generation) = record.generation() {
        write_bytes(b" generation=")?;
        write_decimal(generation.get())?;
    }
    write_bytes(b"\n")
}

fn write_service(service: ServiceId) -> Result<(), Error> {
    if let Some(name) = service_name(service) {
        return write_bytes(name);
    }
    let bytes = service.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            write_bytes(b"-")?;
        }
        let encoded = [hex(byte >> 4), hex(byte & 0x0f)];
        write_bytes(&encoded)?;
    }
    Ok(())
}

const fn observed_name(state: ObservedState) -> &'static [u8] {
    match state {
        ObservedState::Defined => b"defined",
        ObservedState::Activating => b"activating",
        ObservedState::Starting => b"starting",
        ObservedState::Ready => b"ready",
        ObservedState::Degraded => b"degraded",
        ObservedState::Stopping => b"stopping",
        ObservedState::Terminating => b"terminating",
        ObservedState::Stopped => b"stopped",
        ObservedState::Failed => b"failed",
        ObservedState::Quarantined => b"quarantined",
    }
}

const fn desired_name(state: DesiredState) -> &'static [u8] {
    match state {
        DesiredState::Stopped => b"stopped",
        DesiredState::Running => b"running",
    }
}

const fn hex(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        _ => b'a' + value - 10,
    }
}

fn write_decimal(mut value: u64) -> Result<(), Error> {
    let mut bytes = [0_u8; 20];
    let mut start = bytes.len();
    if value == 0 {
        return write_bytes(b"0");
    }
    while value != 0 {
        start -= 1;
        bytes[start] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    write_bytes(&bytes[start..])
}

fn write_bytes(bytes: &[u8]) -> Result<(), Error> {
    syscall::write_all(STDOUT, bytes).map_err(Error::Output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_identity_mapping_is_shared_with_the_transport() {
        assert_eq!(service_id(b"logging"), Some(LOGGING_SERVICE_ID));
        assert_eq!(service_id(b"nullfs"), Some(NULLFS_SERVICE_ID));
        assert_eq!(service_id(b"tmpfs"), Some(TMPFS_SERVICE_ID));
        assert_eq!(service_id(b"vfs"), Some(VFS_SERVICE_ID));
        assert_eq!(service_name(LOGGING_SERVICE_ID), Some(b"logging" as &[u8]));
    }

    #[test]
    fn budgets_are_finite_and_report_each_exhaustion() {
        let mut budgets = Budgets {
            pages: 1,
            requests: 1,
            yields: 0,
        };
        assert_eq!(budgets.spend_page(), Ok(()));
        assert_eq!(budgets.spend_page(), Err(Error::PageBudgetExhausted));
        assert_eq!(budgets.spend_request(), Ok(()));
        assert_eq!(budgets.spend_request(), Err(Error::RequestBudgetExhausted));
        assert_eq!(budgets.yield_once(), Err(Error::YieldBudgetExhausted));
    }

    #[test]
    fn request_ids_are_nonzero_and_monotonic() {
        let mut ids = RequestIds::new();
        assert_eq!(ids.take().unwrap().get(), 1);
        assert_eq!(ids.take().unwrap().get(), 2);
    }

    #[test]
    fn state_names_cover_the_closed_protocol_enums() {
        assert_eq!(observed_name(ObservedState::Ready), b"ready");
        assert_eq!(observed_name(ObservedState::Quarantined), b"quarantined");
        assert_eq!(desired_name(DesiredState::Running), b"running");
        assert_eq!(desired_name(DesiredState::Stopped), b"stopped");
    }
}
