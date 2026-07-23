#![no_std]
#![no_main]

use userspace::{
    abi::{limits, signal},
    environment::Environment,
    platform,
    syscall::{
        self, ChildStatus, DescriptorFlags, FileDescriptor, LaunchBarrier, OpenFlags, PipePair,
        ProcessGroupId, ProcessId, STDERR, STDIN, STDOUT, SpawnFlags,
    },
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const COMMAND_BYTES: usize = 512;
const PATH_BYTES: usize = 256;
const MAX_STAGES: usize = 8;
const MAX_PIPES: usize = MAX_STAGES - 1;
const MAX_JOBS: usize = 4;
const MAX_VARIABLES: usize = limits::MAX_ENVIRONMENT_VARIABLES;
const VARIABLE_NAME_BYTES: usize = limits::MAX_ENVIRONMENT_NAME_BYTES;
const VARIABLE_VALUE_BYTES: usize = COMMAND_BYTES;

const PROMPT: &[u8] = b"ush> ";
const READY: &[u8] = b"userspace shell ready\n";
const HELP: &[u8] = b"builtins: help cd DIRECTORY jobs wait [%N] fg %N bg %N kill %N export NAME[=VALUE] unset NAME env exit\nvariables: NAME=VALUE and $NAME or ${NAME} expansion\nexec: exec <program> [arguments...]\nbackground: command & (up to 4 jobs)\nredirection: < > >> 2> 2>> 2>&1\nCtrl-C: interrupt foreground process group\nCtrl-Z: stop foreground process group\npipeline: producer | filter | consumer (up to 8 stages)\n";
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
const VARIABLE_USAGE: &[u8] = b"ush: expected NAME or NAME=value\n";
const VARIABLE_NAME_FAILURE: &[u8] = b"ush: invalid variable name\n";
const VARIABLE_VALUE_FAILURE: &[u8] = b"ush: variable value is too long\n";
const VARIABLE_LIMIT_FAILURE: &[u8] = b"ush: variable table is full\n";
const VARIABLE_UPDATE_FAILURE: &[u8] = b"ush: environment update failed\n";
const EXPANSION_FAILURE: &[u8] = b"ush: variable expansion failed\n";
const CHANGE_DIRECTORY_USAGE: &[u8] = b"ush: expected exactly one directory\n";
const CHANGE_DIRECTORY_FAILURE: &[u8] = b"ush: cd failed\n";

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
    fn push_byte(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        true
    }
    fn push_bytes(&mut self, bytes: &[u8]) -> bool {
        let Some(end) = self.len.checked_add(bytes.len()) else {
            return false;
        };
        if end > N {
            return false;
        }
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
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
#[derive(Clone, Copy)]
struct Variable {
    name: ByteBuffer<VARIABLE_NAME_BYTES>,
    value: ByteBuffer<VARIABLE_VALUE_BYTES>,
    exported: bool,
}

impl Variable {
    const EMPTY: Self = Self {
        name: ByteBuffer::EMPTY,
        value: ByteBuffer::EMPTY,
        exported: false,
    };

    const fn is_active(self) -> bool {
        self.name.len != 0
    }
}

#[derive(Clone, Copy)]
enum VariableError {
    InvalidName,
    ValueTooLong,
    TableFull,
    System,
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
struct Stage {
    command: ByteBuffer<COMMAND_BYTES>,
    stdin: Option<FileRedirect>,
    stdout: Option<FileRedirect>,
    stderr: Option<FileRedirect>,
    stderr_to_stdout: bool,
}
impl Stage {
    const EMPTY: Self = Self {
        command: ByteBuffer::EMPTY,
        stdin: None,
        stdout: None,
        stderr: None,
        stderr_to_stdout: false,
    };
    fn has_redirection(self) -> bool {
        self.stdin.is_some()
            || self.stdout.is_some()
            || self.stderr.is_some()
            || self.stderr_to_stdout
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
    final_statuses: [Option<ChildStatus>; MAX_STAGES],
    process_count: usize,
    process_group: ProcessGroupId,
    state: JobState,
}
impl Job {
    const EMPTY: Self = Self {
        process_ids: [0; MAX_STAGES],
        final_statuses: [None; MAX_STAGES],
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
    ChangeDirectory,
    Jobs,
    Wait(WaitTarget),
    Foreground(JobTarget),
    Background(JobTarget),
    Export,
    Unset,
    Environment,
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
    variables: [Variable; MAX_VARIABLES],
}
impl Shell {
    fn new(initial_stack: *const usize) -> Self {
        let mut shell = Self {
            command: [0; COMMAND_BYTES],
            pipes: [PipePair {
                reader: 0,
                writer: 0,
            }; MAX_PIPES],
            children: [0; MAX_STAGES],
            jobs: [Job::EMPTY; MAX_JOBS],
            variables: [Variable::EMPTY; MAX_VARIABLES],
        };
        shell.import_environment(initial_stack);
        shell
    }
    fn run(&mut self) -> ! {
        if syscall::write_all(STDOUT, READY).is_err() {
            syscall::exit(1);
        }
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
            let mut parsed = match parse_line(&self.command[..count]) {
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
            if parsed.count == 1 && split_assignment(parsed.stages[0].command.as_slice()).is_some()
            {
                if parsed.background {
                    self.error(BUILTIN_BACKGROUND_FAILURE);
                    continue;
                }
                if parsed.stages[0].has_redirection() {
                    self.error(BUILTIN_REDIRECTION_FAILURE);
                    continue;
                }
                self.run_assignment(parsed.stages[0].command.as_slice());
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
                self.run_builtin(builtin, parsed.stages[0].command.as_slice());
                continue;
            }
            if !self.expand_parsed(&mut parsed) {
                self.error(EXPANSION_FAILURE);
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
    fn run_builtin(&mut self, b: Builtin, command: &[u8]) {
        match b {
            Builtin::Help => self.output(HELP),
            Builtin::ChangeDirectory => {
                self.change_directory(command_arguments(command));
            }
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
            Builtin::Export => self.export_variable(command_arguments(command)),
            Builtin::Unset => self.unset_variable(command_arguments(command)),
            Builtin::Environment => self.print_environment(),
            Builtin::Exit => {
                self.terminate_all_jobs();
                syscall::exit(0)
            }
            Builtin::Kill(JobTarget::Usage) => self.error(JOB_TARGET_USAGE),
            Builtin::Kill(JobTarget::Job(s)) => self.kill_job(s),
        }
    }
    fn import_environment(&mut self, initial_stack: *const usize) {
        let environment = unsafe { Environment::from_stack(initial_stack) };
        for entry in environment.iter() {
            let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
                continue;
            };
            let name = &entry[..separator];
            let value = &entry[separator.saturating_add(1)..];
            if !valid_variable_name(name) || value.len() > VARIABLE_VALUE_BYTES {
                continue;
            }
            let Some(index) = self
                .variables
                .iter()
                .position(|variable| !variable.is_active())
            else {
                break;
            };
            let mut variable = Variable::EMPTY;
            if !variable.name.copy_from(name) || !variable.value.copy_from(value) {
                continue;
            }
            variable.exported = true;
            self.variables[index] = variable;
        }
    }

    fn variable_index(&self, name: &[u8]) -> Option<usize> {
        self.variables
            .iter()
            .position(|variable| variable.is_active() && variable.name.as_slice() == name)
    }

    fn variable_value(&self, name: &[u8]) -> Option<&[u8]> {
        let index = self.variable_index(name)?;
        Some(self.variables[index].value.as_slice())
    }

    fn set_variable(
        &mut self,
        name: &[u8],
        value: &[u8],
        force_export: bool,
    ) -> Result<(), VariableError> {
        if !valid_variable_name(name) {
            return Err(VariableError::InvalidName);
        }
        if value.len() > VARIABLE_VALUE_BYTES {
            return Err(VariableError::ValueTooLong);
        }
        let existing = self.variable_index(name);
        let index = match existing {
            Some(index) => index,
            None => self
                .variables
                .iter()
                .position(|variable| !variable.is_active())
                .ok_or(VariableError::TableFull)?,
        };
        let exported = force_export || existing.is_some_and(|slot| self.variables[slot].exported);
        if exported && syscall::environment_set(name, value).is_err() {
            return Err(VariableError::System);
        }
        let mut variable = Variable::EMPTY;
        if !variable.name.copy_from(name) || !variable.value.copy_from(value) {
            return Err(VariableError::ValueTooLong);
        }
        variable.exported = exported;
        self.variables[index] = variable;
        Ok(())
    }

    fn run_assignment(&mut self, command: &[u8]) {
        let Some((name, value)) = split_assignment(command) else {
            self.error(VARIABLE_NAME_FAILURE);
            return;
        };
        let mut expanded = ByteBuffer::<VARIABLE_VALUE_BYTES>::EMPTY;
        if !self.expand_bytes(value, &mut expanded) {
            self.error(VARIABLE_VALUE_FAILURE);
            return;
        }
        if let Err(error) = self.set_variable(name, expanded.as_slice(), false) {
            self.report_variable_error(error);
        }
    }

    fn export_variable(&mut self, arguments: &[u8]) {
        let arguments = trim_horizontal(arguments);
        if arguments.is_empty() {
            self.error(VARIABLE_USAGE);
            return;
        }
        if let Some((name, value)) = split_assignment(arguments) {
            let mut expanded = ByteBuffer::<VARIABLE_VALUE_BYTES>::EMPTY;
            if !self.expand_bytes(value, &mut expanded) {
                self.error(VARIABLE_VALUE_FAILURE);
                return;
            }
            if let Err(error) = self.set_variable(name, expanded.as_slice(), true) {
                self.report_variable_error(error);
            }
            return;
        }
        if !valid_variable_name(arguments)
            || arguments.iter().any(|byte| is_horizontal_space(*byte))
        {
            self.error(VARIABLE_USAGE);
            return;
        }
        let value = self
            .variable_index(arguments)
            .map(|index| self.variables[index].value)
            .unwrap_or(ByteBuffer::EMPTY);
        if let Err(error) = self.set_variable(arguments, value.as_slice(), true) {
            self.report_variable_error(error);
        }
    }

    fn unset_variable(&mut self, arguments: &[u8]) {
        let name = trim_horizontal(arguments);
        if !valid_variable_name(name) || name.iter().any(|byte| is_horizontal_space(*byte)) {
            self.error(VARIABLE_USAGE);
            return;
        }
        if syscall::environment_unset(name).is_err() {
            self.error(VARIABLE_UPDATE_FAILURE);
            return;
        }
        if let Some(index) = self.variable_index(name) {
            self.variables[index] = Variable::EMPTY;
        }
    }

    fn print_environment(&self) {
        for variable in self
            .variables
            .iter()
            .filter(|variable| variable.is_active() && variable.exported)
        {
            self.output(variable.name.as_slice());
            self.output(b"=");
            self.output(variable.value.as_slice());
            self.output(b"\n");
        }
    }

    fn report_variable_error(&self, error: VariableError) {
        self.error(match error {
            VariableError::InvalidName => VARIABLE_NAME_FAILURE,
            VariableError::ValueTooLong => VARIABLE_VALUE_FAILURE,
            VariableError::TableFull => VARIABLE_LIMIT_FAILURE,
            VariableError::System => VARIABLE_UPDATE_FAILURE,
        });
    }

    fn expand_parsed(&self, parsed: &mut ParsedLine) -> bool {
        for stage in &mut parsed.stages[..parsed.count] {
            let mut command = ByteBuffer::<COMMAND_BYTES>::EMPTY;
            if !self.expand_bytes(stage.command.as_slice(), &mut command) || command.len == 0 {
                return false;
            }
            stage.command = command;
            if let Some(redirect) = &mut stage.stdin
                && !self.expand_redirect(redirect)
            {
                return false;
            }
            if let Some(redirect) = &mut stage.stdout
                && !self.expand_redirect(redirect)
            {
                return false;
            }
            if let Some(redirect) = &mut stage.stderr
                && !self.expand_redirect(redirect)
            {
                return false;
            }
        }
        true
    }

    fn expand_redirect(&self, redirect: &mut FileRedirect) -> bool {
        let mut path = ByteBuffer::<PATH_BYTES>::EMPTY;
        if !self.expand_bytes(redirect.path.as_slice(), &mut path) || path.len == 0 {
            return false;
        }
        redirect.path = path;
        true
    }

    fn expand_bytes<const N: usize>(&self, input: &[u8], output: &mut ByteBuffer<N>) -> bool {
        let mut cursor = 0usize;
        while cursor < input.len() {
            if input[cursor] != b'$' {
                if !output.push_byte(input[cursor]) {
                    return false;
                }
                cursor = cursor.saturating_add(1);
                continue;
            }
            if cursor.saturating_add(1) == input.len() {
                return output.push_byte(b'$');
            }
            if input[cursor.saturating_add(1)] == b'{' {
                let name_start = cursor.saturating_add(2);
                let Some(relative_end) = input[name_start..].iter().position(|byte| *byte == b'}')
                else {
                    return false;
                };
                let name_end = name_start.saturating_add(relative_end);
                let name = &input[name_start..name_end];
                if !valid_variable_name(name) {
                    return false;
                }
                if let Some(value) = self.variable_value(name)
                    && !output.push_bytes(value)
                {
                    return false;
                }
                cursor = name_end.saturating_add(1);
                continue;
            }
            let name_start = cursor.saturating_add(1);
            if !is_variable_name_start(input[name_start]) {
                if !output.push_byte(b'$') {
                    return false;
                }
                cursor = cursor.saturating_add(1);
                continue;
            }
            let mut name_end = name_start.saturating_add(1);
            while name_end < input.len() && is_variable_name_continue(input[name_end]) {
                name_end = name_end.saturating_add(1);
            }
            if let Some(value) = self.variable_value(&input[name_start..name_end])
                && !output.push_bytes(value)
            {
                return false;
            }
            cursor = name_end;
        }
        true
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
        if !self.prepare_pipeline_pipes(pipe_count) {
            self.close_pipes(pipe_count);
            self.error(PIPE_FAILURE);
            return;
        }
        let barrier = match LaunchBarrier::new() {
            Ok(barrier) => barrier,
            Err(_) => {
                self.close_pipes(pipe_count);
                self.error(PIPE_FAILURE);
                return;
            }
        };
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
                    self.abort_pipeline(pipe_count, barrier, stage_index + 1, parsed.count);
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
            let spawned = syscall::spawn_command_with_barrier(
                stage.command.as_slice(),
                flags,
                d.stdin,
                d.stdout,
                d.stderr,
                group,
                &barrier,
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
                    self.abort_pipeline(pipe_count, barrier, stage_index + 1, parsed.count);
                    self.error(SPAWN_FAILURE);
                    return;
                }
            }
        }
        let group = leader.unwrap_or(0);
        if !parsed.background && syscall::foreground_process_group(group).is_err() {
            self.abort_pipeline(pipe_count, barrier, 0, parsed.count);
            self.error(JOB_CONTROL_FAILURE);
            return;
        }
        self.close_pipes(pipe_count);
        if barrier.release().is_err() {
            self.terminate_children(0, parsed.count);
            Self::collect_children(&self.children, parsed.count);
            self.error(PIPE_FAILURE);
            return;
        }
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
        if stage.stderr_to_stdout {
            d.stderr = d.stdout;
        } else if let Some(r) = stage.stderr {
            match open_redirect(r) {
                Ok(fd) => {
                    d.stderr = Some(fd);
                    d.remember(fd)
                }
                Err(()) => {
                    self.close_stage_descriptors(&d);
                    return Err(());
                }
            }
        }
        Ok(d)
    }
    fn close_stage_descriptors(&self, d: &StageDescriptors) {
        for fd in &d.opened[..d.opened_count] {
            let _ = syscall::close(*fd);
        }
    }
    fn finish_foreground(&mut self, process_count: usize, process_group: ProcessGroupId) {
        let mut job = self.make_job(process_count, process_group, JobState::Running);
        let summary = Self::wait_children(&mut job);
        if summary.stopped {
            if let Some(slot) = self.reserve_job() {
                job.state = JobState::Stopped;
                self.jobs[slot] = job;
                self.print_job(slot);
            } else {
                let _ = syscall::signal_process_group(process_group, signal::TERMINATE);
                Self::collect_children(&job.process_ids, job.process_count);
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
            let state = poll_job(&mut self.jobs[slot]);
            self.jobs[slot].state = state;
        }
        if matches!(
            self.jobs[slot].state,
            JobState::Done | JobState::Failed | JobState::Signaled
        ) {
            let _ = self.wait_job(slot);
            return;
        }
        if syscall::foreground_process_group(self.jobs[slot].process_group).is_err() {
            let state = poll_job(&mut self.jobs[slot]);
            self.jobs[slot].state = state;
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
        let summary = Self::wait_children(&mut self.jobs[slot]);
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
            let state = poll_job(&mut self.jobs[slot]);
            self.jobs[slot].state = state;
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
            let state = poll_job(&mut self.jobs[slot]);
            self.jobs[slot].state = state;
        }
        if self.jobs[slot].state == JobState::Stopped {
            self.error(JOB_STOPPED);
            return false;
        }
        match Self::collect_job(&mut self.jobs[slot]) {
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
                let state = poll_job(&mut self.jobs[slot]);
                self.jobs[slot].state = state;
            }
            if self.jobs[slot].state == JobState::Stopped {
                stopped = true;
                continue;
            }
            match Self::collect_job(&mut self.jobs[slot]) {
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

    fn collect_job(job: &mut Job) -> CollectResult {
        let mut failed = false;
        let mut signaled = false;
        for index in 0..job.process_count {
            loop {
                let status = match Self::wait_status(job, index) {
                    Ok(status) => status,
                    Err(_) => return CollectResult::Failed,
                };
                if status.continued() {
                    continue;
                }
                if status.stopped_signal().is_some() {
                    return CollectResult::Stopped;
                }
                job.final_statuses[index] = Some(status);
                classify_status(status, &mut failed, &mut signaled);
                break;
            }
        }
        if failed || signaled {
            CollectResult::Failed
        } else {
            CollectResult::Complete
        }
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
            Self::collect_job(&mut self.jobs[slot]);
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

    fn wait_children(job: &mut Job) -> WaitSummary {
        let mut summary = WaitSummary::default();
        for index in 0..job.process_count {
            match Self::wait_noncontinued(job, index) {
                Ok(status) => {
                    if status.stopped_signal().is_some() {
                        summary.stopped = true;
                    } else if status.interrupted() {
                        summary.interrupted = true;
                    } else if status.signal().is_some() || !status.success() {
                        summary.failed = true;
                    }
                    if !status.continued() && status.stopped_signal().is_none() {
                        job.final_statuses[index] = Some(status);
                    }
                }
                Err(_) => summary.failed = true,
            }
        }
        summary
    }

    fn wait_noncontinued(job: &mut Job, index: usize) -> syscall::Result<ChildStatus> {
        loop {
            let status = Self::wait_status(job, index)?;
            if !status.continued() {
                return Ok(status);
            }
        }
    }

    fn wait_status(job: &Job, index: usize) -> syscall::Result<ChildStatus> {
        match job.final_statuses[index] {
            Some(status) => Ok(status),
            None => syscall::wait_child(job.process_ids[index]),
        }
    }

    fn wait_final(process_id: ProcessId) -> syscall::Result<ChildStatus> {
        loop {
            let status = syscall::wait_child(process_id)?;
            if status.continued() || status.stopped_signal().is_some() {
                continue;
            }
            return Ok(status);
        }
    }

    fn collect_children(process_ids: &[ProcessId; MAX_STAGES], count: usize) {
        for process_id in &process_ids[..count] {
            let _ = Self::wait_final(*process_id);
        }
    }

    fn prepare_pipeline_pipes(&self, count: usize) -> bool {
        for pair in &self.pipes[..count] {
            if syscall::set_descriptor_flags(pair.reader, DescriptorFlags::CLOSE_ON_EXEC).is_err()
                || syscall::set_descriptor_flags(pair.writer, DescriptorFlags::CLOSE_ON_EXEC)
                    .is_err()
            {
                return false;
            }
        }
        true
    }

    fn abort_pipeline(
        &self,
        pipe_count: usize,
        barrier: LaunchBarrier,
        child_start: usize,
        child_end: usize,
    ) {
        self.close_pipes(pipe_count);
        self.terminate_children(child_start, child_end);
        let _ = barrier.release();
        for process_id in &self.children[child_start..child_end] {
            let _ = Self::wait_final(*process_id);
        }
    }

    fn terminate_children(&self, start: usize, end: usize) {
        for process_id in &self.children[start..end] {
            let _ = platform::kill(*process_id, signal::TERMINATE);
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
        self.jobs[slot] = self.make_job(process_count, process_group, state);
    }

    fn make_job(
        &self,
        process_count: usize,
        process_group: ProcessGroupId,
        state: JobState,
    ) -> Job {
        let mut job = Job::EMPTY;
        job.process_count = process_count;
        job.process_group = process_group;
        job.process_ids[..process_count].copy_from_slice(&self.children[..process_count]);
        job.state = state;
        job
    }

    fn print_jobs(&mut self) {
        let mut found = false;
        for slot in 0..MAX_JOBS {
            if !self.jobs[slot].is_active() {
                continue;
            }
            found = true;
            if self.jobs[slot].state == JobState::Running {
                let state = poll_job(&mut self.jobs[slot]);
                self.jobs[slot].state = state;
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

    fn change_directory(&mut self, arguments: &[u8]) {
        let arguments = trim_horizontal(arguments);
        if arguments.is_empty() || arguments.iter().any(|byte| is_horizontal_space(*byte)) {
            self.error(CHANGE_DIRECTORY_USAGE);
            return;
        }

        let mut path = ByteBuffer::<PATH_BYTES>::EMPTY;
        if !self.expand_bytes(arguments, &mut path) || path.as_slice().is_empty() {
            self.error(EXPANSION_FAILURE);
            return;
        }

        if platform::chdir(path.as_slice()).is_err() {
            self.error(CHANGE_DIRECTORY_FAILURE);
            return;
        }

        // Keep the shell's local variable table synchronized with the
        // kernel-managed PWD value without calling environment_set(), because
        // the platform ABI intentionally reserves PWD.
        let mut working_directory = [0_u8; limits::MAX_PATH_BYTES + 1];
        let Ok(working_directory) = platform::getcwd(&mut working_directory) else {
            self.error(CHANGE_DIRECTORY_FAILURE);
            return;
        };

        let index = match self.variable_index(b"PWD") {
            Some(index) => index,
            None => {
                let Some(index) = self
                    .variables
                    .iter()
                    .position(|variable| !variable.is_active())
                else {
                    self.error(VARIABLE_LIMIT_FAILURE);
                    return;
                };
                index
            }
        };

        let mut variable = Variable::EMPTY;
        if !variable.name.copy_from(b"PWD") || !variable.value.copy_from(working_directory) {
            self.error(CHANGE_DIRECTORY_FAILURE);
            return;
        }
        variable.exported = true;
        self.variables[index] = variable;
    }
}

fn poll_job(job: &mut Job) -> JobState {
    let mut running = false;
    let mut stopped = false;
    let mut failed = false;
    let mut signaled = false;
    for index in 0..job.process_count {
        if let Some(status) = job.final_statuses[index] {
            classify_status(status, &mut failed, &mut signaled);
            continue;
        }
        match syscall::try_wait_child(job.process_ids[index]) {
            Ok(status) if status.stopped_signal().is_some() => stopped = true,
            Ok(status) if status.continued() => running = true,
            Ok(status) => {
                job.final_statuses[index] = Some(status);
                classify_status(status, &mut failed, &mut signaled);
            }
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
                    b"2>" | b"2>>" => {
                        stage.stderr = Some(r);
                        stage.stderr_to_stdout = false;
                    }
                    _ => return Err(ParseError::RedirectionSyntax),
                }
            }
            b"2>&1" => {
                stage.stderr = None;
                stage.stderr_to_stdout = true;
            }
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
    syscall::open(r.path.as_slice(), flags | OpenFlags::CLOSE_ON_EXEC).map_err(|_| ())
}

fn detect_builtin(command: &[u8]) -> Option<Builtin> {
    let token_end = command
        .iter()
        .position(|byte| is_horizontal_space(*byte))
        .unwrap_or(command.len());
    let arguments = &command[token_end..];
    match &command[..token_end] {
        b"help" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => Some(Builtin::Help),
        b"cd" => Some(Builtin::ChangeDirectory),
        b"jobs" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => Some(Builtin::Jobs),
        b"exit" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => Some(Builtin::Exit),
        b"wait" => Some(Builtin::Wait(parse_wait_target(arguments))),
        b"fg" => Some(Builtin::Foreground(parse_job_target(arguments))),
        b"bg" => Some(Builtin::Background(parse_job_target(arguments))),
        b"kill" => Some(Builtin::Kill(parse_job_target(arguments))),
        b"export" => Some(Builtin::Export),
        b"unset" => Some(Builtin::Unset),
        b"env" if arguments.iter().all(|byte| is_horizontal_space(*byte)) => {
            Some(Builtin::Environment)
        }
        _ => None,
    }
}

fn command_arguments(command: &[u8]) -> &[u8] {
    let token_end = command
        .iter()
        .position(|byte| is_horizontal_space(*byte))
        .unwrap_or(command.len());
    &command[token_end..]
}

fn is_variable_name_start(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'_')
}

fn is_variable_name_continue(byte: u8) -> bool {
    is_variable_name_start(byte) || byte.is_ascii_digit()
}

fn valid_variable_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= VARIABLE_NAME_BYTES
        && is_variable_name_start(name[0])
        && name[1..]
            .iter()
            .all(|byte| is_variable_name_continue(*byte))
}

fn split_assignment(command: &[u8]) -> Option<(&[u8], &[u8])> {
    if command.iter().any(|byte| is_horizontal_space(*byte)) {
        return None;
    }
    let separator = command.iter().position(|byte| *byte == b'=')?;
    let name = &command[..separator];
    valid_variable_name(name).then_some((name, &command[separator.saturating_add(1)..]))
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

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let mut shell = Shell::new(initial_stack);
    shell.run()
}
