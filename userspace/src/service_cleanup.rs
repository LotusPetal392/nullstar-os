//! Allocation-free classification and diagnostics for PID 1 cleanup operations.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Definition,
    Logging,
    Tmpfs,
    Vfs,
}

impl Service {
    const fn name(self) -> &'static [u8] {
        match self {
            Self::Definition => b"definition",
            Self::Logging => b"logging",
            Self::Tmpfs => b"tmpfs",
            Self::Vfs => b"vfs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    LeaderReap,
    UnassignedProcessGroup,
    EmptyJobInspection,
    JobDrain,
    ResourceRelease,
}

impl Phase {
    const fn name(self) -> &'static [u8] {
        match self {
            Self::LeaderReap => b"leader-reap",
            Self::UnassignedProcessGroup => b"unassigned-process-group",
            Self::EmptyJobInspection => b"empty-job-inspection",
            Self::JobDrain => b"job-drain",
            Self::ResourceRelease => b"resource-release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    SignalProcessGroup,
    TryWaitChild,
    JobTerminate,
    JobTryWait,
    CapabilityClose,
    LaunchBarrierRelease,
    YieldNow,
}

impl Operation {
    const fn name(self) -> &'static [u8] {
        match self {
            Self::SignalProcessGroup => b"signal-process-group",
            Self::TryWaitChild => b"try-wait-child",
            Self::JobTerminate => b"job-terminate",
            Self::JobTryWait => b"job-try-wait",
            Self::CapabilityClose => b"capability-close",
            Self::LaunchBarrierRelease => b"launch-barrier-release",
            Self::YieldNow => b"yield-now",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    UnexpectedSuccess { value: u64, auxiliary: u64 },
    Error { code: i32 },
    MissingHandle,
    BudgetExhausted { attempts: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diagnostic {
    pub service: Service,
    pub phase: Phase,
    pub operation: Operation,
    pub observation: Observation,
}

impl Diagnostic {
    pub fn encode(self, output: &mut [u8]) -> Option<usize> {
        let mut encoder = Encoder::new(output);
        encoder.push(b"init: cleanup service=")?;
        encoder.push(self.service.name())?;
        encoder.push(b" phase=")?;
        encoder.push(self.phase.name())?;
        encoder.push(b" operation=")?;
        encoder.push(self.operation.name())?;
        match self.observation {
            Observation::UnexpectedSuccess { value, auxiliary } => {
                encoder.push(b" result=unexpected-success value=")?;
                encoder.push_u64(value)?;
                encoder.push(b" auxiliary=")?;
                encoder.push_u64(auxiliary)?;
            }
            Observation::Error { code } => {
                encoder.push(b" result=error code=")?;
                encoder.push_i32(code)?;
            }
            Observation::MissingHandle => {
                encoder.push(b" result=missing-handle")?;
            }
            Observation::BudgetExhausted { attempts } => {
                encoder.push(b" result=budget-exhausted attempts=")?;
                encoder.push_u64(u64::from(attempts))?;
            }
        }
        encoder.push(b"\n")?;
        Some(encoder.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Progress,
    Pending,
    Complete,
    Retry,
    Unexpected(Observation),
}

pub fn classify_group_signal(result: Result<usize, i32>, no_child: i32) -> Action {
    match result {
        Ok(0) => Action::Unexpected(Observation::UnexpectedSuccess {
            value: 0,
            auxiliary: 0,
        }),
        Ok(_) => Action::Progress,
        Err(code) if code == no_child => Action::Complete,
        Err(code) => Action::Unexpected(Observation::Error { code }),
    }
}

pub fn classify_job_terminate(result: Result<usize, i32>) -> Action {
    match result {
        Ok(_) => Action::Progress,
        Err(code) => Action::Unexpected(Observation::Error { code }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobWaitResult {
    Exit { process_id: u64, status: u64 },
    Error(i32),
}

pub fn classify_job_wait(
    result: JobWaitResult,
    try_again: i32,
    no_child: i32,
    expect_empty: bool,
) -> Action {
    match result {
        JobWaitResult::Exit { process_id, status } if expect_empty => {
            Action::Unexpected(Observation::UnexpectedSuccess {
                value: process_id,
                auxiliary: status,
            })
        }
        JobWaitResult::Exit { .. } => Action::Progress,
        JobWaitResult::Error(code) if code == no_child => Action::Complete,
        JobWaitResult::Error(code) if code == try_again && !expect_empty => Action::Pending,
        JobWaitResult::Error(code) => Action::Unexpected(Observation::Error { code }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderResult {
    Terminal,
    Transitional,
    Error(i32),
}

pub fn classify_leader_wait(
    result: LeaderResult,
    try_again: i32,
    interrupted: i32,
    no_child: i32,
) -> Action {
    match result {
        LeaderResult::Terminal => Action::Complete,
        LeaderResult::Transitional => Action::Pending,
        LeaderResult::Error(code) if code == interrupted => Action::Retry,
        LeaderResult::Error(code) if code == try_again => Action::Pending,
        LeaderResult::Error(code) if code == no_child => Action::Complete,
        LeaderResult::Error(code) => Action::Unexpected(Observation::Error { code }),
    }
}

pub fn classify_unit(result: Result<(), i32>) -> Action {
    match result {
        Ok(()) => Action::Complete,
        Err(code) => Action::Unexpected(Observation::Error { code }),
    }
}

struct Encoder<'a> {
    output: &'a mut [u8],
    length: usize,
}

impl<'a> Encoder<'a> {
    const fn new(output: &'a mut [u8]) -> Self {
        Self { output, length: 0 }
    }

    const fn len(&self) -> usize {
        self.length
    }

    fn push(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.length.checked_add(bytes.len())?;
        let destination = self.output.get_mut(self.length..end)?;
        destination.copy_from_slice(bytes);
        self.length = end;
        Some(())
    }

    fn push_u64(&mut self, mut value: u64) -> Option<()> {
        let mut digits = [0_u8; 20];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.push(&digits[start..])
    }

    fn push_i32(&mut self, value: i32) -> Option<()> {
        if value < 0 {
            self.push(b"-")?;
        }
        self.push_u64(u64::from(value.unsigned_abs()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, Diagnostic, JobWaitResult, LeaderResult, Observation, Operation, Phase, Service,
        classify_group_signal, classify_job_terminate, classify_job_wait, classify_leader_wait,
        classify_unit,
    };

    const TRY_AGAIN: i32 = -11;
    const INTERRUPTED: i32 = -4;
    const NO_CHILD: i32 = -10;

    #[test]
    fn group_signal_distinguishes_empty_progress_zero_and_errors() {
        assert_eq!(classify_group_signal(Ok(3), NO_CHILD), Action::Progress);
        assert_eq!(
            classify_group_signal(Err(NO_CHILD), NO_CHILD),
            Action::Complete
        );
        assert_eq!(
            classify_group_signal(Ok(0), NO_CHILD),
            Action::Unexpected(Observation::UnexpectedSuccess {
                value: 0,
                auxiliary: 0,
            })
        );
        assert_eq!(
            classify_group_signal(Err(-5), NO_CHILD),
            Action::Unexpected(Observation::Error { code: -5 })
        );
    }

    #[test]
    fn job_terminate_accepts_zero_but_rejects_errors() {
        assert_eq!(classify_job_terminate(Ok(0)), Action::Progress);
        assert_eq!(classify_job_terminate(Ok(4)), Action::Progress);
        assert_eq!(
            classify_job_terminate(Err(-9)),
            Action::Unexpected(Observation::Error { code: -9 })
        );
    }

    #[test]
    fn job_wait_distinguishes_drain_and_empty_inspection() {
        let exit = JobWaitResult::Exit {
            process_id: 42,
            status: 137,
        };
        assert_eq!(
            classify_job_wait(exit, TRY_AGAIN, NO_CHILD, false),
            Action::Progress
        );
        assert_eq!(
            classify_job_wait(exit, TRY_AGAIN, NO_CHILD, true),
            Action::Unexpected(Observation::UnexpectedSuccess {
                value: 42,
                auxiliary: 137,
            })
        );
        assert_eq!(
            classify_job_wait(JobWaitResult::Error(TRY_AGAIN), TRY_AGAIN, NO_CHILD, false),
            Action::Pending
        );
        assert_eq!(
            classify_job_wait(JobWaitResult::Error(TRY_AGAIN), TRY_AGAIN, NO_CHILD, true),
            Action::Unexpected(Observation::Error { code: TRY_AGAIN })
        );
        assert_eq!(
            classify_job_wait(JobWaitResult::Error(NO_CHILD), TRY_AGAIN, NO_CHILD, false),
            Action::Complete
        );
    }

    #[test]
    fn leader_wait_preserves_retry_pending_and_completion_semantics() {
        assert_eq!(
            classify_leader_wait(LeaderResult::Terminal, TRY_AGAIN, INTERRUPTED, NO_CHILD),
            Action::Complete
        );
        assert_eq!(
            classify_leader_wait(LeaderResult::Transitional, TRY_AGAIN, INTERRUPTED, NO_CHILD,),
            Action::Pending
        );
        assert_eq!(
            classify_leader_wait(
                LeaderResult::Error(INTERRUPTED),
                TRY_AGAIN,
                INTERRUPTED,
                NO_CHILD,
            ),
            Action::Retry
        );
        assert_eq!(
            classify_leader_wait(
                LeaderResult::Error(TRY_AGAIN),
                TRY_AGAIN,
                INTERRUPTED,
                NO_CHILD,
            ),
            Action::Pending
        );
        assert_eq!(
            classify_leader_wait(
                LeaderResult::Error(NO_CHILD),
                TRY_AGAIN,
                INTERRUPTED,
                NO_CHILD,
            ),
            Action::Complete
        );
    }

    #[test]
    fn unit_results_preserve_exact_error_codes() {
        assert_eq!(classify_unit(Ok(())), Action::Complete);
        assert_eq!(
            classify_unit(Err(-9)),
            Action::Unexpected(Observation::Error { code: -9 })
        );
    }

    #[test]
    fn diagnostic_encoding_is_canonical_and_handles_integer_boundaries() {
        let diagnostic = Diagnostic {
            service: Service::Logging,
            phase: Phase::JobDrain,
            operation: Operation::JobTryWait,
            observation: Observation::UnexpectedSuccess {
                value: u64::MAX,
                auxiliary: 0,
            },
        };
        let mut output = [0_u8; 192];
        let length = diagnostic.encode(&mut output).expect("diagnostic fits");
        assert_eq!(
            &output[..length],
            b"init: cleanup service=logging phase=job-drain operation=job-try-wait result=unexpected-success value=18446744073709551615 auxiliary=0\n"
        );

        let error = Diagnostic {
            service: Service::Definition,
            phase: Phase::ResourceRelease,
            operation: Operation::CapabilityClose,
            observation: Observation::Error { code: i32::MIN },
        };
        let length = error.encode(&mut output).expect("error diagnostic fits");
        assert_eq!(
            &output[..length],
            b"init: cleanup service=definition phase=resource-release operation=capability-close result=error code=-2147483648\n"
        );

        let tmpfs = Diagnostic {
            service: Service::Tmpfs,
            phase: Phase::JobDrain,
            operation: Operation::JobTryWait,
            observation: Observation::MissingHandle,
        };
        let length = tmpfs.encode(&mut output).expect("diagnostic fits");
        assert_eq!(
            &output[..length],
            b"init: cleanup service=tmpfs phase=job-drain operation=job-try-wait result=missing-handle\n"
        );

        let vfs = Diagnostic {
            service: Service::Vfs,
            phase: Phase::JobDrain,
            operation: Operation::JobTryWait,
            observation: Observation::MissingHandle,
        };
        let length = vfs.encode(&mut output).expect("diagnostic fits");
        assert_eq!(
            &output[..length],
            b"init: cleanup service=vfs phase=job-drain operation=job-try-wait result=missing-handle\n"
        );
    }

    #[test]
    fn diagnostic_encoding_reports_small_buffer_exhaustion() {
        let diagnostic = Diagnostic {
            service: Service::Logging,
            phase: Phase::JobDrain,
            operation: Operation::YieldNow,
            observation: Observation::BudgetExhausted { attempts: 64 },
        };
        let mut output = [0_u8; 16];
        assert_eq!(diagnostic.encode(&mut output), None);
    }
}
