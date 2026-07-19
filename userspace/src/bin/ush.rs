#![no_std]
#![no_main]

use userspace::{
    abi::signal,
    syscall::{
        self, ChildStatus, PipePair, ProcessGroupId, ProcessId, STDERR, STDIN, STDOUT, SpawnFlags,
    },
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const COMMAND_BYTES: usize = 512;
const MAX_STAGES: usize = 8;
const MAX_PIPES: usize = MAX_STAGES - 1;
const MAX_JOBS: usize = 4;

const PROMPT: &[u8] = b"ush> ";
const HELP: &[u8] = b"builtins: help jobs wait kill %N exit\nbackground: command & (up to 4 jobs)\nCtrl-C: interrupt foreground process group\npipeline: producer | filter | consumer (up to 8 stages)\n";
const SYNTAX_FAILURE: &[u8] = b"ush: expected a non-empty pipeline stage\n";
const STAGE_FAILURE: &[u8] = b"ush: pipeline supports at most 8 stages\n";
const JOB_LIMIT_FAILURE: &[u8] = b"ush: background job table is full\n";
const BUILTIN_BACKGROUND_FAILURE: &[u8] = b"ush: builtins cannot run in the background\n";
const KILL_USAGE: &[u8] = b"usage: kill %1..%4\n";
const KILL_MISSING: &[u8] = b"ush: background job not found\n";
const KILL_FAILURE: &[u8] = b"ush: kill failed\n";
const INTERRUPTED: &[u8] = b"ush: interrupted\n";
const PIPE_FAILURE: &[u8] = b"ush: pipe failed\n";
const SPAWN_FAILURE: &[u8] = b"ush: spawn failed\n";
const WAIT_FAILURE: &[u8] = b"ush: wait failed\n";
const WAIT_COMPLETE: &[u8] = b"ush: background jobs complete\n";
const NO_JOBS: &[u8] = b"ush: no background jobs\n";

#[derive(Clone, Copy)]
struct Stage {
    start: usize,
    end: usize,
}

impl Stage {
    const EMPTY: Self = Self { start: 0, end: 0 };
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
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
    Wait,
    Exit,
    Kill(KillTarget),
}

#[derive(Clone, Copy)]
enum KillTarget {
    Job(usize),
    Usage,
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
                Ok(count) => count,
                Err(_) => syscall::exit(1),
            };
            if count == 0 {
                let _ = self.wait_all_jobs();
                syscall::exit(0);
            }

            let parsed = match parse_line(&self.command[..count]) {
                Ok(parsed) => parsed,
                Err(ParseError::EmptyStage) => {
                    self.error(SYNTAX_FAILURE);
                    continue;
                }
                Err(ParseError::TooManyStages) => {
                    self.error(STAGE_FAILURE);
                    continue;
                }
            };
            if parsed.count == 0 {
                continue;
            }

            let builtin = if parsed.count == 1 {
                let stage = parsed.stages[0];
                detect_builtin(&self.command[stage.start..stage.end])
            } else {
                None
            };
            if let Some(builtin) = builtin {
                if parsed.background {
                    self.error(BUILTIN_BACKGROUND_FAILURE);
                    continue;
                }
                self.run_builtin(builtin);
                continue;
            }

            let job_slot = if parsed.background {
                match self.reserve_job() {
                    Some(slot) => Some(slot),
                    None => {
                        self.error(JOB_LIMIT_FAILURE);
                        continue;
                    }
                }
            } else {
                None
            };

            if parsed.count == 1 {
                self.run_single(parsed, job_slot);
            } else {
                self.run_pipeline(parsed, job_slot);
            }
        }
    }

    fn run_builtin(&mut self, builtin: Builtin) {
        match builtin {
            Builtin::Help => self.output(HELP),
            Builtin::Jobs => self.print_jobs(),
            Builtin::Wait => {
                if self.wait_all_jobs() {
                    self.output(WAIT_COMPLETE);
                } else {
                    self.error(WAIT_FAILURE);
                }
            }
            Builtin::Exit => {
                let _ = self.wait_all_jobs();
                syscall::exit(0);
            }
            Builtin::Kill(KillTarget::Usage) => self.error(KILL_USAGE),
            Builtin::Kill(KillTarget::Job(slot)) => self.kill_job(slot),
        }
    }

    fn run_single(&mut self, parsed: ParsedLine, job_slot: Option<usize>) {
        let stage = parsed.stages[0];
        let command = &self.command[stage.start..stage.end];
        let mut flags = SpawnFlags::NEW_PROCESS_GROUP;
        if !parsed.background {
            flags |= SpawnFlags::FOREGROUND;
        }

        let process_id = match syscall::spawn_command(command, flags, None, None, None) {
            Ok(process_id) => process_id,
            Err(_) => {
                self.error(SPAWN_FAILURE);
                return;
            }
        };

        if let Some(slot) = job_slot {
            let mut job = Job::EMPTY;
            job.process_ids[0] = process_id;
            job.process_count = 1;
            job.process_group = process_id;
            job.state = JobState::Started;
            self.jobs[slot] = job;
            self.print_job(slot);
            self.jobs[slot].state = JobState::Running;
            return;
        }

        match syscall::wait_child(process_id) {
            Ok(status) if status.interrupted() => self.error(INTERRUPTED),
            Ok(_) => {}
            Err(_) => self.error(WAIT_FAILURE),
        }
    }

    fn run_pipeline(&mut self, parsed: ParsedLine, job_slot: Option<usize>) {
        let pipe_count = parsed.count - 1;
        let mut created = 0usize;
        while created < pipe_count {
            match syscall::pipe_pair() {
                Ok(pair) => {
                    self.pipes[created] = pair;
                    created += 1;
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
            let command = &self.command[stage.start..stage.end];

            let mut flags = SpawnFlags::USE_DESCRIPTORS;
            let process_group = if stage_index == parsed.count - 1 {
                flags |= SpawnFlags::NEW_PROCESS_GROUP;
                if !parsed.background {
                    flags |= SpawnFlags::FOREGROUND;
                }
                None
            } else {
                flags |= SpawnFlags::JOIN_PROCESS_GROUP;
                leader
            };
            let stdin_descriptor = if stage_index == 0 {
                None
            } else {
                Some(self.pipes[stage_index - 1].reader)
            };
            let stdout_descriptor = if stage_index == parsed.count - 1 {
                None
            } else {
                Some(self.pipes[stage_index].writer)
            };

            match syscall::spawn_command(
                command,
                flags,
                stdin_descriptor,
                stdout_descriptor,
                process_group,
            ) {
                Ok(process_id) => {
                    self.children[stage_index] = process_id;
                    if leader.is_none() {
                        leader = Some(process_id);
                    }
                }
                Err(_) => {
                    self.close_pipes(pipe_count);
                    for child_index in stage_index + 1..parsed.count {
                        let _ = syscall::wait_child(self.children[child_index]);
                    }
                    self.error(SPAWN_FAILURE);
                    return;
                }
            }
        }

        self.close_pipes(pipe_count);
        let process_group = leader.unwrap_or(0);
        if let Some(slot) = job_slot {
            let mut job = Job::EMPTY;
            job.process_count = parsed.count;
            job.process_group = process_group;
            job.process_ids[..parsed.count].copy_from_slice(&self.children[..parsed.count]);
            job.state = JobState::Started;
            self.jobs[slot] = job;
            self.print_job(slot);
            self.jobs[slot].state = JobState::Running;
            return;
        }

        let mut wait_failed = false;
        let mut interrupted = false;
        for process_id in &self.children[..parsed.count] {
            match syscall::wait_child(*process_id) {
                Ok(status) => interrupted |= status.interrupted(),
                Err(_) => wait_failed = true,
            }
        }
        if wait_failed {
            self.error(WAIT_FAILURE);
        } else if interrupted {
            self.error(INTERRUPTED);
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

    fn print_jobs(&mut self) {
        let mut found = false;
        for slot in 0..MAX_JOBS {
            if !self.jobs[slot].is_active() {
                continue;
            }
            found = true;
            if self.jobs[slot].state == JobState::Running {
                let state = poll_job(&self.jobs[slot]);
                self.jobs[slot].state = state;
            }
            self.print_job(slot);
        }
        if !found {
            self.output(NO_JOBS);
        }
    }

    fn wait_all_jobs(&mut self) -> bool {
        let mut succeeded = true;
        for job in &mut self.jobs {
            if !job.is_active() {
                continue;
            }
            for process_id in &job.process_ids[..job.process_count] {
                if syscall::wait_child(*process_id).is_err() {
                    succeeded = false;
                }
            }
            *job = Job::EMPTY;
        }
        succeeded
    }

    fn kill_job(&mut self, slot: usize) {
        if slot >= MAX_JOBS || !self.jobs[slot].is_active() {
            self.error(KILL_MISSING);
            return;
        }
        if syscall::signal_process_group(self.jobs[slot].process_group, signal::TERMINATE).is_err()
        {
            self.error(KILL_FAILURE);
            return;
        }
        self.jobs[slot].state = JobState::Signaled;
        self.print_job(slot);
    }

    fn print_job(&self, slot: usize) {
        const DIGITS: &[u8] = b"1234";
        self.output(b"[");
        self.output(&DIGITS[slot..slot + 1]);
        self.output(b"] ");
        let status = match self.jobs[slot].state {
            JobState::Running => b"running\n" as &[u8],
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
    let mut failed = false;
    let mut signaled = false;
    for process_id in &job.process_ids[..job.process_count] {
        match syscall::try_wait_child(*process_id) {
            Ok(status) => classify_status(status, &mut failed, &mut signaled),
            Err(error) if error == syscall::Errno::TRY_AGAIN => running = true,
            Err(_) => failed = true,
        }
    }

    if running {
        JobState::Running
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
    let mut stage_start = 0usize;
    let mut cursor = 0usize;
    loop {
        if cursor == end || bytes[cursor] == b'|' {
            let mut start = stage_start;
            let mut finish = cursor;
            while start < finish && is_horizontal_space(bytes[start]) {
                start += 1;
            }
            while finish > start && is_horizontal_space(bytes[finish - 1]) {
                finish -= 1;
            }
            if start == finish {
                return Err(ParseError::EmptyStage);
            }
            if parsed.count == MAX_STAGES {
                return Err(ParseError::TooManyStages);
            }
            parsed.stages[parsed.count] = Stage { start, end: finish };
            parsed.count += 1;

            if cursor == end {
                break;
            }
            stage_start = cursor + 1;
        }
        cursor += 1;
    }
    Ok(parsed)
}

fn detect_builtin(command: &[u8]) -> Option<Builtin> {
    match command {
        b"help" => Some(Builtin::Help),
        b"jobs" => Some(Builtin::Jobs),
        b"wait" => Some(Builtin::Wait),
        b"exit" => Some(Builtin::Exit),
        _ => {
            let token_end = command
                .iter()
                .position(|byte| is_horizontal_space(*byte))
                .unwrap_or(command.len());
            if &command[..token_end] != b"kill" {
                return None;
            }
            Some(Builtin::Kill(parse_kill_target(&command[token_end..])))
        }
    }
}

fn parse_kill_target(mut bytes: &[u8]) -> KillTarget {
    while let Some((first, rest)) = bytes.split_first() {
        if !is_horizontal_space(*first) {
            break;
        }
        bytes = rest;
    }
    if bytes.first() == Some(&b'%') {
        bytes = &bytes[1..];
    }
    if bytes.len() != 1 || !(b'1'..=b'4').contains(&bytes[0]) {
        return KillTarget::Usage;
    }
    KillTarget::Job(usize::from(bytes[0] - b'1'))
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
