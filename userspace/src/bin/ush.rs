#![no_std]
#![no_main]

use userspace::{
    abi::signal,
    syscall::{
        self, ChildStatus, FileDescriptor, OpenFlags, PipePair, ProcessGroupId, ProcessId, STDERR,
        STDIN, STDOUT, SpawnFlags,
    },
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const COMMAND_BYTES: usize = 512;
const PATH_BYTES: usize = 256;
const MAX_STAGES: usize = 8;
const MAX_PIPES: usize = MAX_STAGES - 1;
const MAX_JOBS: usize = 4;

const PROMPT: &[u8] = b"ush> ";
const HELP: &[u8] = b"builtins: help jobs wait [%N] fg %N bg %N kill %N exit\nexec: exec <program> [arguments...]\nbackground: command & (up to 4 jobs)\nredirection: < > >> 2> 2>> 2>&1\nCtrl-C: interrupt foreground process group\nCtrl-Z: stop foreground process group\npipeline: producer | filter | consumer (up to 8 stages)\n";
const SYNTAX_FAILURE: &[u8] = b"ush: expected a non-empty pipeline stage\n";
const STAGE_FAILURE: &[u8] = b"ush: pipeline supports at most 8 stages\n";
const REDIRECTION_SYNTAX_FAILURE: &[u8] = b"ush: invalid redirection syntax\n";
const REDIRECTION_PATH_FAILURE: &[u8] = b"ush: redirection path is too long\n";
const REDIRECTION_OPEN_FAILURE: &[u8] = b"ush: redirection open failed\n";
const BUILTIN_REDIRECTION_FAILURE: &[u8] = b"ush: builtins do not support redirection\n";
const JOB_LIMIT_FAILURE: &[u8] = b"ush: background job table is full\n";
const BUILTIN_BACKGROUND_FAILURE: &[u8] = b"ush: builtins cannot run in the background\n";
const JOB_TARGET_USAGE: &[u8] = b"ush: expected a job selector from %1 to %4\n";
const JOB_MISSING: &[u8] = b"ush: background job not found\n";
const JOB_NOT_STOPPED: &[u8] = b"ush: background job is not stopped\n";
const JOB_STOPPED: &[u8] = b"ush: background job is stopped\n";
const JOB_CONTROL_FAILURE: &[u8] = b"ush: job control failed\n";
const INTERRUPTED: &[u8] = b"ush: interrupted\n";
const PIPE_FAILURE: &[u8] = b"ush: pipe failed\n";
const SPAWN_FAILURE: &[u8] = b"ush: spawn failed\n";
const WAIT_FAILURE: &[u8] = b"ush: wait failed\n";
const WAIT_COMPLETE: &[u8] = b"ush: background jobs complete\n";
const NO_JOBS: &[u8] = b"ush: no background jobs\n";

#[derive(Clone, Copy)]
struct ByteBuffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
}
impl<const N: usize> ByteBuffer<N> {
    const EMPTY: Self = Self {
        bytes: [0; N],
        len: 0,
    };
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    fn copy_from(&mut self, source: &[u8]) -> bool {
        if source.len() > N {
            return false;
        }
        self.bytes[..source.len()].copy_from_slice(source);
        self.len = source.len();
        true
    }
    fn push_token(&mut self, token: &[u8]) -> bool {
        let separator = usize::from(self.len != 0);
        let Some(end) = self
            .len
            .checked_add(separator)
            .and_then(|v| v.checked_add(token.len()))
        else {
            return false;
        };
        if end > N {
            return false;
        }
        if separator != 0 {
            self.bytes[self.len] = b' ';
            self.len += 1;
        }
        self.bytes[self.len..self.len + token.len()].copy_from_slice(token);
        self.len += token.len();
        true
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RedirectMode {
    Read,
    Truncate,
    Append,
}
#[derive(Clone, Copy)]
struct FileRedirect {
    path: ByteBuffer<PATH_BYTES>,
    mode: RedirectMode,
}
#[derive(Clone, Copy)]
enum StderrRedirect {
    Default,
    File(FileRedirect),
    Stdout,
}
#[derive(Clone, Copy)]
struct Stage {
    command: ByteBuffer<COMMAND_BYTES>,
    stdin: Option<FileRedirect>,
    stdout: Option<FileRedirect>,
    stderr: StderrRedirect,
}
impl Stage {
    const EMPTY: Self = Self {
        command: ByteBuffer::EMPTY,
        stdin: None,
        stdout: None,
        stderr: StderrRedirect::Default,
    };
    fn has_redirection(self) -> bool {
        self.stdin.is_some()
            || self.stdout.is_some()
            || !matches!(self.stderr, StderrRedirect::Default)
    }
}
#[derive(Clone, Copy)]
struct ParsedLine {
    stages: [Stage; MAX_STAGES],
    count: usize,
    background: bool,
}
impl ParsedLine {
    const EMPTY: Self = Self {
        stages: [Stage::EMPTY; MAX_STAGES],
        count: 0,
        background: false,
    };
}
#[derive(Clone, Copy)]
enum ParseError {
    EmptyStage,
    TooManyStages,
    RedirectionSyntax,
    PathTooLong,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Stopped,
    Done,
    Failed,
    Started,
    Signaled,
}
#[derive(Clone, Copy)]
struct Job {
    process_ids: [ProcessId; MAX_STAGES],
    process_count: usize,
    process_group: ProcessGroupId,
    state: JobState,
}
impl Job {
    const EMPTY: Self = Self {
        process_ids: [0; MAX_STAGES],
        process_count: 0,
        process_group: 0,
        state: JobState::Running,
    };
    const fn is_active(self) -> bool {
        self.process_count != 0
    }
}
#[derive(Clone, Copy)]
enum Builtin {
    Help,
    Jobs,
    Wait(WaitTarget),
    Foreground(JobTarget),
    Background(JobTarget),
    Exit,
    Kill(JobTarget),
}
#[derive(Clone, Copy)]
enum JobTarget {
    Job(usize),
    Usage,
}
#[derive(Clone, Copy)]
enum WaitTarget {
    All,
    Job(usize),
    Usage,
}
#[derive(Clone, Copy, Default)]
struct WaitSummary {
    failed: bool,
    interrupted: bool,
    stopped: bool,
}
enum WaitAllResult {
    Complete,
    Stopped,
    Failed,
}
enum CollectResult {
    Complete,
    Stopped,
    Failed,
}
#[derive(Clone, Copy)]
struct StageDescriptors {
    stdin: Option<FileDescriptor>,
    stdout: Option<FileDescriptor>,
    stderr: Option<FileDescriptor>,
    opened: [FileDescriptor; 3],
    opened_count: usize,
}
impl StageDescriptors {
    const fn new(stdin: Option<FileDescriptor>, stdout: Option<FileDescriptor>) -> Self {
        Self {
            stdin,
            stdout,
            stderr: None,
            opened: [0; 3],
            opened_count: 0,
        }
    }
    fn remember(&mut self, d: FileDescriptor) {
        self.opened[self.opened_count] = d;
        self.opened_count += 1;
    }
    const fn uses_descriptors(self) -> bool {
        self.stdin.is_some() || self.stdout.is_some() || self.stderr.is_some()
    }
}
struct Shell {
    command: [u8; COMMAND_BYTES],
    pipes: [PipePair; MAX_PIPES],
    children: [ProcessId; MAX_STAGES],
    jobs: [Job; MAX_JOBS],
}
impl Shell {
    const fn new() -> Self {
        Self {
            command: [0; COMMAND_BYTES],
            pipes: [PipePair {
                reader: 0,
                writer: 0,
            }; MAX_PIPES],
            children: [0; MAX_STAGES],
            jobs: [Job::EMPTY; MAX_JOBS],
        }
    }
    fn run(&mut self) -> ! {
        loop {
            if syscall::write_all(STDOUT, PROMPT).is_err() {
                syscall::exit(1);
            }
            let count = match syscall::read(STDIN, &mut self.command) {
                Ok(c) => c,
                Err(_) => syscall::exit(1),
            };
            if count == 0 {
                self.terminate_all_jobs();
                syscall::exit(0);
            }
            let parsed = match parse_line(&self.command[..count]) {
                Ok(v) => v,
                Err(ParseError::EmptyStage) => {
                    self.error(SYNTAX_FAILURE);
                    continue;
                }
                Err(ParseError::TooManyStages) => {
                    self.error(STAGE_FAILURE);
                    continue;
                }
                Err(ParseError::RedirectionSyntax) => {
                    self.error(REDIRECTION_SYNTAX_FAILURE);
                    continue;
                }
                Err(ParseError::PathTooLong) => {
                    self.error(REDIRECTION_PATH_FAILURE);
                    continue;
                }
            };
            if parsed.count == 0 {
                continue;
            }
            let builtin = if parsed.count == 1 {
                detect_builtin(parsed.stages[0].command.as_slice())
            } else {
                None
            };
            if let Some(builtin) = builtin {
                if parsed.background {
                    self.error(BUILTIN_BACKGROUND_FAILURE);
                    continue;
                }
                if parsed.stages[0].has_redirection() {
                    self.error(BUILTIN_REDIRECTION_FAILURE);
                    continue;
                }
                self.run_builtin(builtin);
                continue;
            }
            let slot = if parsed.background {
                match self.reserve_job() {
                    Some(s) => Some(s),
                    None => {
                        self.error(JOB_LIMIT_FAILURE);
                        continue;
                    }
                }
            } else {
                None
            };
            if parsed.count == 1 {
                self.run_single(parsed, slot)
            } else {
                self.run_pipeline(parsed, slot)
            }
        }
    }
    fn run_builtin(&mut self, b: Builtin) {
        match b {
            Builtin::Help => self.output(HELP),
            Builtin::Jobs => self.print_jobs(),
            Builtin::Wait(WaitTarget::All) => match self.wait_all_jobs() {
                WaitAllResult::Complete => self.output(WAIT_COMPLETE),
                WaitAllResult::Stopped => self.error(JOB_STOPPED),
                WaitAllResult::Failed => self.error(WAIT_FAILURE),
            },
            Builtin::Wait(WaitTarget::Job(s)) => {
                if self.wait_job(s) {
                    self.output(WAIT_COMPLETE);
                }
            }
            Builtin::Wait(WaitTarget::Usage) => self.error(JOB_TARGET_USAGE),
            Builtin::Foreground(JobTarget::Job(s)) => self.foreground_job(s),
            Builtin::Foreground(JobTarget::Usage) => self.error(JOB_TARGET_USAGE),
            Builtin::Background(JobTarget::Job(s)) => self.background_job(s),
            Builtin::Background(JobTarget::Usage) => self.error(JOB_TARGET_USAGE),
            Builtin::Exit => {
                self.terminate_all_jobs();
                syscall::exit(0)
            }
            Builtin::Kill(JobTarget::Usage) => self.error(JOB_TARGET_USAGE),
            Builtin::Kill(JobTarget::Job(s)) => self.kill_job(s),
        }
    }
    fn run_single(&mut self, parsed: ParsedLine, job_slot: Option<usize>) {
        let stage = parsed.stages[0];
        let d = match self.open_stage(&stage, None, None) {
            Ok(v) => v,
            Err(()) => {
                self.error(REDIRECTION_OPEN_FAILURE);
                return;
            }
        };
        let mut flags = SpawnFlags::NEW_PROCESS_GROUP;
        if !parsed.background {
            flags |= SpawnFlags::FOREGROUND;
        }
        if d.uses_descriptors() {
            flags |= SpawnFlags::USE_DESCRIPTORS;
        }
        let spawned = syscall::spawn_command(
            stage.command.as_slice(),
            flags,
            d.stdin,
            d.stdout,
            d.stderr,
            None,
        );
        self.close_stage_descriptors(&d);
        let pid = match spawned {
            Ok(p) => p,
            Err(_) => {
                self.error(SPAWN_FAILURE);
                return;
            }
        };
        self.children[0] = pid;
        if let Some(slot) = job_slot {
            self.store_job(slot, 1, pid, JobState::Started);
            self.print_job(slot);
            self.jobs[slot].state = JobState::Running;
            return;
        }
        self.finish_foreground(1, pid)
    }
    fn run_pipeline(&mut self, parsed: ParsedLine, job_slot: Option<usize>) {
        let pipe_count = parsed.count - 1;
        let mut created = 0;
        while created < pipe_count {
            match syscall::pipe_pair() {
                Ok(pair) => {
                    self.pipes[created] = pair;
                    created += 1
                }
                Err(_) => {
                    self.close_pipes(created);
                    self.error(PIPE_FAILURE);
                    return;
                }
            }
        }
        let mut leader = None;
        let mut stage_index = parsed.count;
        while stage_index > 0 {
            stage_index -= 1;
            let stage = parsed.stages[stage_index];
            let default_stdin = if stage_index == 0 {
                None
            } else {
                Some(self.pipes[stage_index - 1].reader)
            };
            let default_stdout = if stage_index == parsed.count - 1 {
                None
            } else {
                Some(self.pipes[stage_index].writer)
            };
            let d = match self.open_stage(&stage, default_stdin, default_stdout) {
                Ok(v) => v,
                Err(()) => {
                    self.close_pipes(pipe_count);
                    for i in stage_index + 1..parsed.count {
                        let _ = self.wait_final(self.children[i]);
                    }
                    self.error(REDIRECTION_OPEN_FAILURE);
                    return;
                }
            };
            let mut flags = SpawnFlags::EMPTY;
            if d.uses_descriptors() {
                flags |= SpawnFlags::USE_DESCRIPTORS;
            }
            let group = if stage_index == parsed.count - 1 {
                flags |= SpawnFlags::NEW_PROCESS_GROUP;
                if !parsed.background {
                    flags |= SpawnFlags::FOREGROUND;
                }
                None
            } else {
                flags |= SpawnFlags::JOIN_PROCESS_GROUP;
                leader
            };
            let spawned = syscall::spawn_command(
                stage.command.as_slice(),
                flags,
                d.stdin,
                d.stdout,
                d.stderr,
                group,
            );
            self.close_stage_descriptors(&d);
            match spawned {
                Ok(pid) => {
                    self.children[stage_index] = pid;
                    if leader.is_none() {
                        leader = Some(pid);
                    }
                }
                Err(_) => {
                    self.close_pipes(pipe_count);
                    for i in stage_index + 1..parsed.count {
                        let _ = self.wait_final(self.children[i]);
                    }
                    self.error(SPAWN_FAILURE);
                    return;
                }
            }
        }
        self.close_pipes(pipe_count);
        let group = leader.unwrap_or(0);
        if let Some(slot) = job_slot {
            self.store_job(slot, parsed.count, group, JobState::Started);
            self.print_job(slot);
            self.jobs[slot].state = JobState::Running;
            return;
        }
        self.finish_foreground(parsed.count, group)
    }
    fn open_stage(
        &self,
        stage: &Stage,
        default_stdin: Option<FileDescriptor>,
        default_stdout: Option<FileDescriptor>,
    ) -> Result<StageDescriptors, ()> {
        let mut d = StageDescriptors::new(default_stdin, default_stdout);
        if let Some(r) = stage.stdin {
            match open_redirect(r) {
                Ok(fd) => {
                    d.stdin = Some(fd);
                    d.remember(fd)
                }
                Err(()) => return Err(()),
            }
        }
        if let Some(r) = stage.stdout {
            match open_redirect(r) {
                Ok(fd) => {
                    d.stdout = Some(fd);
                    d.remember(fd)
                }
                Err(()) => {
                    self.close_stage_descriptors(&d);
                    return Err(());
                }
            }
        }
        match stage.stderr {
            StderrRedirect::Default => {}
            StderrRedirect::Stdout => d.stderr = d.stdout,
            StderrRedirect::File(r) => match open_redirect(r) {
                Ok(fd) => {
                    d.stderr = Some(fd);
                    d.remember(fd)
                }
                Err(()) => {
                    self.close_stage_descriptors(&d);
                    return Err(());
                }
            },
        }
        Ok(d)
    }
    fn close_stage_descriptors(&self, d: &StageDescriptors) {
        for fd in &d.opened[..d.opened_count] {
            let _ = syscall::close(*fd);
        }
    }
    fn finish_foreground(&mut self, process_count: usize, process_group: ProcessGroupId) {
        let children = self.children;
        let summary = self.wait_children(&children, process_count);
        if summary.stopped {
            if let Some(slot) = self.reserve_job() {
                self.store_job(slot, process_count, process_group, JobState::Stopped);
                self.print_job(slot);
            } else {
                let _ = syscall::signal_process_group(process_group, signal::TERMINATE);
                self.collect_children(&children, process_count);
                self.error(JOB_LIMIT_FAILURE);
            }
        } else if summary.failed {
            self.error(WAIT_FAILURE);
        } else if summary.interrupted {
            self.error(INTERRUPTED);
        }
    }

    fn foreground_job(&mut self, slot: usize) {
        if !self.valid_job(slot) {
            self.error(JOB_MISSING);
            return;
        }
        if self.jobs[slot].state == JobState::Running {
            self.jobs[slot].state = poll_job(&self.jobs[slot]);
        }
        let job = self.jobs[slot];
        if matches!(
            job.state,
            JobState::Done | JobState::Failed | JobState::Signaled
        ) {
            let _ = self.wait_job(slot);
            return;
        }
        if syscall::foreground_process_group(job.process_group).is_err() {
            self.jobs[slot].state = poll_job(&self.jobs[slot]);
            if matches!(
                self.jobs[slot].state,
                JobState::Done | JobState::Failed | JobState::Signaled
            ) {
                let _ = self.wait_job(slot);
            } else {
                self.error(JOB_CONTROL_FAILURE);
            }
            return;
        }

        self.jobs[slot].state = JobState::Running;
        let summary = self.wait_children(&job.process_ids, job.process_count);
        if summary.stopped {
            self.jobs[slot].state = JobState::Stopped;
            self.print_job(slot);
            return;
        }
        self.jobs[slot] = Job::EMPTY;
        if summary.failed {
            self.error(WAIT_FAILURE);
        } else if summary.interrupted {
            self.error(INTERRUPTED);
        }
    }

    fn background_job(&mut self, slot: usize) {
        if !self.valid_job(slot) {
            self.error(JOB_MISSING);
            return;
        }
        if self.jobs[slot].state == JobState::Running {
            self.jobs[slot].state = poll_job(&self.jobs[slot]);
        }
        if self.jobs[slot].state != JobState::Stopped {
            self.error(JOB_NOT_STOPPED);
            return;
        }
        if syscall::signal_process_group(self.jobs[slot].process_group, signal::CONTINUE).is_err() {
            self.error(JOB_CONTROL_FAILURE);
            return;
        }
        self.jobs[slot].state = JobState::Running;
        self.print_job(slot);
    }

    fn wait_job(&mut self, slot: usize) -> bool {
        if !self.valid_job(slot) {
            self.error(JOB_MISSING);
            return false;
        }
        if self.jobs[slot].state == JobState::Running {
            self.jobs[slot].state = poll_job(&self.jobs[slot]);
        }
        if self.jobs[slot].state == JobState::Stopped {
            self.error(JOB_STOPPED);
            return false;
        }
        let job = self.jobs[slot];
        match self.collect_job(&job) {
            CollectResult::Complete => {
                self.jobs[slot] = Job::EMPTY;
                true
            }
            CollectResult::Stopped => {
                self.jobs[slot].state = JobState::Stopped;
                self.error(JOB_STOPPED);
                false
            }
            CollectResult::Failed => {
                self.jobs[slot] = Job::EMPTY;
                self.error(WAIT_FAILURE);
                false
            }
        }
    }

    fn wait_all_jobs(&mut self) -> WaitAllResult {
        let mut failed = false;
        let mut stopped = false;
        for slot in 0..MAX_JOBS {
            if !self.jobs[slot].is_active() {
                continue;
            }
            if self.jobs[slot].state == JobState::Running {
                self.jobs[slot].state = poll_job(&self.jobs[slot]);
            }
            if self.jobs[slot].state == JobState::Stopped {
                stopped = true;
                continue;
            }
            let job = self.jobs[slot];
            match self.collect_job(&job) {
                CollectResult::Complete => self.jobs[slot] = Job::EMPTY,
                CollectResult::Stopped => {
                    self.jobs[slot].state = JobState::Stopped;
                    stopped = true;
                }
                CollectResult::Failed => {
                    self.jobs[slot] = Job::EMPTY;
                    failed = true;
                }
            }
        }
        if failed {
            WaitAllResult::Failed
        } else if stopped {
            WaitAllResult::Stopped
        } else {
            WaitAllResult::Complete
        }
    }

    fn collect_job(&self, job: &Job) -> CollectResult {
        for process_id in &job.process_ids[..job.process_count] {
            loop {
                let status = match syscall::wait_child(*process_id) {
                    Ok(status) => status,
                    Err(_) => return CollectResult::Failed,
                };
                if status.continued() {
                    continue;
                }
                if status.stopped_signal().is_some() {
                    return CollectResult::Stopped;
                }
                break;
            }
        }
        CollectResult::Complete
    }

    fn terminate_all_jobs(&mut self) {
        for job in &self.jobs {
            if job.is_active() {
                let _ = syscall::signal_process_group(job.process_group, signal::TERMINATE);
            }
        }
        for slot in 0..MAX_JOBS {
            if !self.jobs[slot].is_active() {
                continue;
            }
            let job = self.jobs[slot];
            self.collect_children(&job.process_ids, job.process_count);
            self.jobs[slot] = Job::EMPTY;
        }
    }

    fn kill_job(&mut self, slot: usize) {
        if !self.valid_job(slot) {
            self.error(JOB_MISSING);
            return;
        }
        if syscall::signal_process_group(self.jobs[slot].process_group, signal::TERMINATE).is_err()
        {
            self.error(JOB_CONTROL_FAILURE);
            return;
        }
        self.jobs[slot].state = JobState::Signaled;
        self.print_job(slot);
    }

    fn wait_children(&self, process_ids: &[ProcessId; MAX_STAGES], count: usize) -> WaitSummary {
        let mut summary = WaitSummary::default();
        for process_id in &process_ids[..count] {
            match self.wait_noncontinued(*process_id) {
                Ok(status) => {
                    if status.stopped_signal().is_some() {
                        summary.stopped = true;
                    } else if status.interrupted() {
                        summary.interrupted = true;
                    } else if status.signal().is_some() || !status.success() {
                        summary.failed = true;
                    }
                }
                Err(_) => summary.failed = true,
            }
        }
        summary
    }

    fn wait_noncontinued(&self, process_id: ProcessId) -> syscall::Result<ChildStatus> {
        loop {
            let status = syscall::wait_child(process_id)?;
            if !status.continued() {
                return Ok(status);
            }
        }
    }

    fn wait_final(&self, process_id: ProcessId) -> syscall::Result<ChildStatus> {
        loop {
            let status = syscall::wait_child(process_id)?;
            if status.continued() || status.stopped_signal().is_some() {
                continue;
            }
            return Ok(status);
        }
    }

    fn collect_children(&self, process_ids: &[ProcessId; MAX_STAGES], count: usize) {
        for process_id in &process_ids[..count] {
            let _ = self.wait_final(*process_id);
        }
    }

    fn close_pipes(&self, count: usize) {
        for pair in &self.pipes[..count] {
            let _ = syscall::close(pair.reader);
            let _ = syscall::close(pair.writer);
        }
    }

    fn reserve_job(&self) -> Option<usize> {
        self.jobs.iter().position(|job| !job.is_active())
    }

    fn valid_job(&self, slot: usize) -> bool {
        slot < MAX_JOBS && self.jobs[slot].is_active()
    }

    fn store_job(
        &mut self,
        slot: usize,
        process_count: usize,
        process_group: ProcessGroupId,
        state: JobState,
    ) {
        let mut job = Job::EMPTY;
        job.process_count = process_count;
        job.process_group = process_group;
        job.process_ids[..process_count].copy_from_slice(&self.children[..process_count]);
        job.state = state;
        self.jobs[slot] = job;
    }

    fn print_jobs(&mut self) {
        let mut found = false;
        for slot in 0..MAX_JOBS {
            if !self.jobs[slot].is_active() {
                continue;
            }
            found = true;
            if self.jobs[slot].state == JobState::Running {
                self.jobs[slot].state = poll_job(&self.jobs[slot]);
            }
            self.print_job(slot);
        }
        if !found {
            self.output(NO_JOBS);
        }
    }

    fn print_job(&self, slot: usize) {
        const DIGITS: &[u8] = b"1234";
        self.output(b"[");
        self.output(&DIGITS[slot..slot + 1]);
        self.output(b"] ");
        let status = match self.jobs[slot].state {
            JobState::Running => b"running\n" as &[u8],
            JobState::Stopped => b"stopped\n",
            JobState::Done => b"done\n",
            JobState::Failed => b"failed\n",
            JobState::Started => b"started\n",
            JobState::Signaled => b"signaled\n",
        };
        self.output(status);
    }

    fn output(&self, bytes: &[u8]) {
        let _ = syscall::write_all(STDOUT, bytes);
    }

    fn error(&self, bytes: &[u8]) {
        let _ = syscall::write_all(STDERR, bytes);
    }
}

fn poll_job(job: &Job) -> JobState {
    let mut running = false;
    let mut stopped = false;
    let mut failed = false;
    let mut signaled = false;
    for process_id in &job.process_ids[..job.process_count] {
        match syscall::try_wait_child(*process_id) {
            Ok(status) if status.stopped_signal().is_some() => stopped = true,
            Ok(status) => classify_status(status, &mut failed, &mut signaled),
            Err(error) if error == syscall::Errno::TRY_AGAIN => running = true,
            Err(_) => failed = true,
        }
    }

    if running {
        JobState::Running
    } else if stopped {
        JobState::Stopped
    } else if signaled {
        JobState::Signaled
    } else if failed {
        JobState::Failed
    } else {
        JobState::Done
    }
}

fn classify_status(status: ChildStatus, failed: &mut bool, signaled: &mut bool) {
    if status.signal().is_some() {
        *signaled = true;
    } else if !status.success() {
        *failed = true;
    }
}

fn parse_line(bytes: &[u8]) -> Result<ParsedLine, ParseError> {
    let mut end = bytes.len();
    while end > 0 && is_line_trailing(bytes[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return Ok(ParsedLine::EMPTY);
    }
    let mut background = false;
    if bytes[end - 1] == b'&' {
        background = true;
        end -= 1;
        while end > 0 && is_horizontal_space(bytes[end - 1]) {
            end -= 1;
        }
        if end == 0 {
            return Err(ParseError::EmptyStage);
        }
    }
    let mut parsed = ParsedLine {
        background,
        ..ParsedLine::EMPTY
    };
    let mut start = 0;
    let mut cursor = 0;
    loop {
        if cursor == end || bytes[cursor] == b'|' {
            let mut a = start;
            let mut b = cursor;
            while a < b && is_horizontal_space(bytes[a]) {
                a += 1;
            }
            while b > a && is_horizontal_space(bytes[b - 1]) {
                b -= 1;
            }
            if a == b {
                return Err(ParseError::EmptyStage);
            }
            if parsed.count == MAX_STAGES {
                return Err(ParseError::TooManyStages);
            }
            parsed.stages[parsed.count] = parse_stage(&bytes[a..b])?;
            parsed.count += 1;
            if cursor == end {
                break;
            }
            start = cursor + 1;
        }
        cursor += 1;
    }
    Ok(parsed)
}
fn parse_stage(bytes: &[u8]) -> Result<Stage, ParseError> {
    let mut stage = Stage::EMPTY;
    let mut cursor = 0;
    while let Some(token) = next_token(bytes, &mut cursor) {
        match token {
            b"<" | b">" | b">>" | b"2>" | b"2>>" => {
                let path = next_token(bytes, &mut cursor).ok_or(ParseError::RedirectionSyntax)?;
                if is_redirection_operator(path) {
                    return Err(ParseError::RedirectionSyntax);
                }
                let mut stored = ByteBuffer::<PATH_BYTES>::EMPTY;
                if !stored.copy_from(path) {
                    return Err(ParseError::PathTooLong);
                }
                let r = FileRedirect {
                    path: stored,
                    mode: match token {
                        b"<" => RedirectMode::Read,
                        b">" | b"2>" => RedirectMode::Truncate,
                        b">>" | b"2>>" => RedirectMode::Append,
                        _ => return Err(ParseError::RedirectionSyntax),
                    },
                };
                match token {
                    b"<" => stage.stdin = Some(r),
                    b">" | b">>" => stage.stdout = Some(r),
                    b"2>" | b"2>>" => stage.stderr = StderrRedirect::File(r),
                    _ => return Err(ParseError::RedirectionSyntax),
                }
            }
            b"2>&1" => stage.stderr = StderrRedirect::Stdout,
            _ => {
                if !stage.command.push_token(token) {
                    return Err(ParseError::RedirectionSyntax);
                }
            }
        }
    }
    if stage.command.len == 0 {
        return Err(ParseError::EmptyStage);
    }
    Ok(stage)
}
fn next_token<'a>(bytes: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    while *cursor < bytes.len() && is_horizontal_space(bytes[*cursor]) {
        *cursor += 1;
    }
    if *cursor == bytes.len() {
        return None;
    }
    let start = *cursor;
    while *cursor < bytes.len() && !is_horizontal_space(bytes[*cursor]) {
        *cursor += 1;
    }
    Some(&bytes[start..*cursor])
}
fn is_redirection_operator(token: &[u8]) -> bool {
    matches!(token, b"<" | b">" | b">>" | b"2>" | b"2>>" | b"2>&1")
}
fn open_redirect(r: FileRedirect) -> Result<FileDescriptor, ()> {
    let flags = match r.mode {
        RedirectMode::Read => OpenFlags::READ,
        RedirectMode::Truncate => OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        RedirectMode::Append => OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::APPEND,
    };
    syscall::open(r.path.as_slice(), flags).map_err(|_| ())
}

fn detect_builtin(command: &[u8]) -> Option<Builtin> {
    let token_end = command
        .iter()
        .position(|byte| is_horizontal_space(*byte))
        .unwrap_or(command.len());
    let arguments = &command[token_end..];
    match &command[..token_end] {
        b"help" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => Some(Builtin::Help),
        b"jobs" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => Some(Builtin::Jobs),
        b"exit" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => Some(Builtin::Exit),
        b"wait" => Some(Builtin::Wait(parse_wait_target(arguments))),
        b"fg" => Some(Builtin::Foreground(parse_job_target(arguments))),
        b"bg" => Some(Builtin::Background(parse_job_target(arguments))),
        b"kill" => Some(Builtin::Kill(parse_job_target(arguments))),
        _ => None,
    }
}

fn parse_wait_target(bytes: &[u8]) -> WaitTarget {
    let bytes = trim_horizontal(bytes);
    if bytes.is_empty() {
        WaitTarget::All
    } else {
        match parse_job_target(bytes) {
            JobTarget::Job(slot) => WaitTarget::Job(slot),
            JobTarget::Usage => WaitTarget::Usage,
        }
    }
}

fn parse_job_target(bytes: &[u8]) -> JobTarget {
    let mut bytes = trim_horizontal(bytes);
    if bytes.first() == Some(&b'%') {
        bytes = &bytes[1..];
    }
    if bytes.len() != 1 || !(b'1'..=b'4').contains(&bytes[0]) {
        return JobTarget::Usage;
    }
    JobTarget::Job(usize::from(bytes[0] - b'1'))
}

fn trim_horizontal(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if !is_horizontal_space(*first) {
            break;
        }
        bytes = rest;
    }
    while let Some((last, rest)) = bytes.split_last() {
        if !is_horizontal_space(*last) {
            break;
        }
        bytes = rest;
    }
    bytes
}

const fn is_horizontal_space(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

const fn is_line_trailing(byte: u8) -> bool {
    is_horizontal_space(byte) || byte == b'\n' || byte == b'\r'
}

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let mut shell = Shell::new();
    shell.run()
}
