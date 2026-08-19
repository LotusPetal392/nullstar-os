//! Bounded process and thread identity/lifecycle state.
//!
//! This module is the execution-foundation state machine.  A process owns
//! hierarchy and lifetime, while threads own schedulable state and exit
//! observations.  The scheduler can attach architecture-specific context to
//! these identities without changing the lifecycle rules.

use alloc::vec::Vec;

pub const MAX_PROCESSES: usize = 64;
pub const MAX_THREADS: usize = 128;
pub const MAX_THREADS_PER_PROCESS: usize = 16;
pub const MAX_CHILDREN_PER_PROCESS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcessId(u64);

impl ProcessId {
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ThreadId(u64);

impl ThreadId {
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Created,
    Running,
    Exited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadState {
    Runnable,
    Running,
    Blocked,
    Sleeping,
    Stopped,
    Exited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadExit {
    pub thread_id: ThreadId,
    pub process_id: ProcessId,
    pub status: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    pub process_id: ProcessId,
    pub status: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadSnapshot {
    pub id: ThreadId,
    pub process_id: ProcessId,
    pub name: &'static str,
    pub state: ThreadState,
    pub detached: bool,
    pub exit: Option<ThreadExit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub id: ProcessId,
    pub parent: Option<ProcessId>,
    pub state: ProcessState,
    pub thread_count: usize,
    pub live_thread_count: usize,
    pub child_count: usize,
    pub exit: Option<ProcessExit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateError {
    ProcessLimitReached,
    ThreadLimitReached,
    InvalidProcess,
    ProcessExited,
    ThreadLimitPerProcessReached,
    ChildLimitReached,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupError {
    InvalidProcess,
    InvalidThread,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    InvalidThread,
    InvalidTransition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinError {
    InvalidThread,
    NotExited,
    Detached,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReapError {
    InvalidProcess,
    NotExited,
    HasThreads,
    HasChildren,
}

struct Thread {
    id: ThreadId,
    process_id: ProcessId,
    name: &'static str,
    state: ThreadState,
    detached: bool,
    exit: Option<ThreadExit>,
}

impl Thread {
    fn snapshot(&self) -> ThreadSnapshot {
        ThreadSnapshot {
            id: self.id,
            process_id: self.process_id,
            name: self.name,
            state: self.state,
            detached: self.detached,
            exit: self.exit,
        }
    }
}

struct Process {
    id: ProcessId,
    parent: Option<ProcessId>,
    state: ProcessState,
    threads: Vec<Thread>,
    children: Vec<ProcessId>,
    exit: Option<ProcessExit>,
}

impl Process {
    fn snapshot(&self) -> ProcessSnapshot {
        ProcessSnapshot {
            id: self.id,
            parent: self.parent,
            state: self.state,
            thread_count: self.threads.len(),
            live_thread_count: self
                .threads
                .iter()
                .filter(|thread| thread.state != ThreadState::Exited)
                .count(),
            child_count: self.children.len(),
            exit: self.exit,
        }
    }
}

/// Bounded process hierarchy and thread lifecycle registry.
pub struct ProcessTable {
    next_process_id: u64,
    next_thread_id: u64,
    processes: Vec<Process>,
    thread_count: usize,
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            next_process_id: 1,
            next_thread_id: 1,
            processes: Vec::new(),
            thread_count: 0,
        }
    }

    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    pub fn thread_count(&self) -> usize {
        self.thread_count
    }

    pub fn create_process(&mut self, parent: Option<ProcessId>) -> Result<ProcessId, CreateError> {
        if self.processes.len() >= MAX_PROCESSES {
            return Err(CreateError::ProcessLimitReached);
        }
        if let Some(parent_id) = parent {
            let parent_process = self
                .processes
                .iter()
                .find(|process| process.id == parent_id)
                .ok_or(CreateError::InvalidProcess)?;
            if parent_process.state == ProcessState::Exited {
                return Err(CreateError::ProcessExited);
            }
            if parent_process.children.len() >= MAX_CHILDREN_PER_PROCESS {
                return Err(CreateError::ChildLimitReached);
            }
        }

        let id = ProcessId(self.next_process_id);
        self.next_process_id = self.next_process_id.saturating_add(1);
        self.processes.push(Process {
            id,
            parent,
            state: ProcessState::Created,
            threads: Vec::new(),
            children: Vec::new(),
            exit: None,
        });
        if let Some(parent_id) = parent {
            self.process_mut(parent_id)
                .expect("validated process parent disappeared")
                .children
                .push(id);
        }
        Ok(id)
    }

    pub fn create_thread(
        &mut self,
        process_id: ProcessId,
        name: &'static str,
    ) -> Result<ThreadId, CreateError> {
        if self.thread_count >= MAX_THREADS {
            return Err(CreateError::ThreadLimitReached);
        }
        let process_index = self
            .processes
            .iter()
            .position(|process| process.id == process_id)
            .ok_or(CreateError::InvalidProcess)?;
        let process = &self.processes[process_index];
        if process.state == ProcessState::Exited {
            return Err(CreateError::ProcessExited);
        }
        if process.threads.len() >= MAX_THREADS_PER_PROCESS {
            return Err(CreateError::ThreadLimitPerProcessReached);
        }

        let id = ThreadId(self.next_thread_id);
        self.next_thread_id = self.next_thread_id.saturating_add(1);
        let process = &mut self.processes[process_index];
        process.threads.push(Thread {
            id,
            process_id,
            name,
            state: ThreadState::Runnable,
            detached: false,
            exit: None,
        });
        process.state = ProcessState::Running;
        self.thread_count += 1;
        Ok(id)
    }

    pub fn process_snapshot(&self, process_id: ProcessId) -> Result<ProcessSnapshot, LookupError> {
        self.process(process_id)
            .map(Process::snapshot)
            .ok_or(LookupError::InvalidProcess)
    }

    pub fn thread_snapshot(&self, thread_id: ThreadId) -> Result<ThreadSnapshot, LookupError> {
        self.thread(thread_id)
            .map(Thread::snapshot)
            .ok_or(LookupError::InvalidThread)
    }

    pub fn threads(&self, process_id: ProcessId) -> Result<Vec<ThreadSnapshot>, LookupError> {
        let process = self
            .process(process_id)
            .ok_or(LookupError::InvalidProcess)?;
        Ok(process.threads.iter().map(Thread::snapshot).collect())
    }

    pub fn run_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if thread.state != ThreadState::Runnable {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Running;
        Ok(())
    }

    pub fn yield_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if thread.state != ThreadState::Running {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Runnable;
        Ok(())
    }

    pub fn block_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if !matches!(thread.state, ThreadState::Runnable | ThreadState::Running) {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Blocked;
        Ok(())
    }

    pub fn sleep_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if !matches!(thread.state, ThreadState::Runnable | ThreadState::Running) {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Sleeping;
        Ok(())
    }

    pub fn wake_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if !matches!(thread.state, ThreadState::Blocked | ThreadState::Sleeping) {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Runnable;
        Ok(())
    }

    pub fn stop_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if !matches!(
            thread.state,
            ThreadState::Runnable
                | ThreadState::Running
                | ThreadState::Blocked
                | ThreadState::Sleeping
        ) {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Stopped;
        Ok(())
    }

    pub fn continue_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let thread = self
            .thread_mut(thread_id)
            .ok_or(StateError::InvalidThread)?;
        if thread.state != ThreadState::Stopped {
            return Err(StateError::InvalidTransition);
        }
        thread.state = ThreadState::Runnable;
        Ok(())
    }

    pub fn exit_thread(
        &mut self,
        thread_id: ThreadId,
        status: u64,
    ) -> Result<Option<ProcessExit>, StateError> {
        let (process_id, detached) = {
            let thread = self
                .thread_mut(thread_id)
                .ok_or(StateError::InvalidThread)?;
            if thread.state == ThreadState::Exited {
                return Err(StateError::InvalidTransition);
            }
            thread.state = ThreadState::Exited;
            thread.exit = Some(ThreadExit {
                thread_id,
                process_id: thread.process_id,
                status,
            });
            (thread.process_id, thread.detached)
        };

        let process = self
            .process_mut(process_id)
            .ok_or(StateError::InvalidThread)?;
        let all_exited = process
            .threads
            .iter()
            .all(|thread| thread.state == ThreadState::Exited);
        let process_exit = if all_exited {
            let process_exit = ProcessExit { process_id, status };
            process.state = ProcessState::Exited;
            process.exit = Some(process_exit);
            Some(process_exit)
        } else {
            None
        };

        if detached {
            self.reclaim_exited_thread(process_id, thread_id);
        }
        Ok(process_exit)
    }

    pub fn detach_thread(&mut self, thread_id: ThreadId) -> Result<(), StateError> {
        let (process_id, exited) = {
            let thread = self
                .thread_mut(thread_id)
                .ok_or(StateError::InvalidThread)?;
            if thread.detached {
                return Err(StateError::InvalidTransition);
            }
            thread.detached = true;
            (thread.process_id, thread.state == ThreadState::Exited)
        };
        if exited {
            self.reclaim_exited_thread(process_id, thread_id);
        }
        Ok(())
    }

    pub fn join_thread(&mut self, thread_id: ThreadId) -> Result<ThreadExit, JoinError> {
        let (process_id, exit) = {
            let thread = self.thread(thread_id).ok_or(JoinError::InvalidThread)?;
            if thread.detached {
                return Err(JoinError::Detached);
            }
            let exit = thread.exit.ok_or(JoinError::NotExited)?;
            (thread.process_id, exit)
        };
        self.reclaim_exited_thread(process_id, thread_id);
        Ok(exit)
    }

    pub fn reap_process(&mut self, process_id: ProcessId) -> Result<ProcessExit, ReapError> {
        let process = self.process(process_id).ok_or(ReapError::InvalidProcess)?;
        if process.state != ProcessState::Exited {
            return Err(ReapError::NotExited);
        }
        if !process.threads.is_empty() {
            return Err(ReapError::HasThreads);
        }
        if !process.children.is_empty() {
            return Err(ReapError::HasChildren);
        }
        let exit = process.exit.expect("exited process has an exit record");
        let parent = process.parent;
        let index = self
            .processes
            .iter()
            .position(|candidate| candidate.id == process_id)
            .expect("validated process disappeared");
        self.processes.remove(index);
        if let Some(parent_id) = parent {
            if let Some(parent) = self.process_mut(parent_id) {
                parent.children.retain(|child| *child != process_id);
            }
        }
        Ok(exit)
    }

    fn reclaim_exited_thread(&mut self, process_id: ProcessId, thread_id: ThreadId) {
        let process = self
            .process_mut(process_id)
            .expect("thread owner disappeared before thread reclamation");
        let index = process
            .threads
            .iter()
            .position(|thread| thread.id == thread_id && thread.state == ThreadState::Exited)
            .expect("only exited threads can be reclaimed");
        process.threads.remove(index);
        self.thread_count = self.thread_count.saturating_sub(1);
    }

    fn process(&self, process_id: ProcessId) -> Option<&Process> {
        self.processes
            .iter()
            .find(|process| process.id == process_id)
    }

    fn process_mut(&mut self, process_id: ProcessId) -> Option<&mut Process> {
        self.processes
            .iter_mut()
            .find(|process| process.id == process_id)
    }

    fn thread(&self, thread_id: ThreadId) -> Option<&Thread> {
        self.processes
            .iter()
            .find_map(|process| process.threads.iter().find(|thread| thread.id == thread_id))
    }

    fn thread_mut(&mut self, thread_id: ThreadId) -> Option<&mut Thread> {
        self.processes.iter_mut().find_map(|process| {
            process
                .threads
                .iter_mut()
                .find(|thread| thread.id == thread_id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_hierarchy_and_multi_thread_lifecycle_are_independent() {
        let mut table = ProcessTable::new();
        let parent = table.create_process(None).unwrap();
        let child = table.create_process(Some(parent)).unwrap();
        let first = table.create_thread(child, "worker-a").unwrap();
        let second = table.create_thread(child, "worker-b").unwrap();

        table.run_thread(first).unwrap();
        table.block_thread(first).unwrap();
        table.run_thread(second).unwrap();
        assert_eq!(
            table.process_snapshot(child).unwrap().state,
            ProcessState::Running
        );

        table.exit_thread(first, 7).unwrap();
        assert_eq!(table.process_snapshot(child).unwrap().live_thread_count, 1);
        assert_eq!(table.join_thread(first).unwrap().status, 7);

        let process_exit = table.exit_thread(second, 9).unwrap().unwrap();
        assert_eq!(process_exit.status, 9);
        assert_eq!(
            table.process_snapshot(child).unwrap().state,
            ProcessState::Exited
        );
        assert_eq!(table.join_thread(second).unwrap().status, 9);
        assert_eq!(table.reap_process(child).unwrap().process_id, child);
        assert_eq!(table.process_snapshot(parent).unwrap().child_count, 0);
    }

    #[test]
    fn blocked_sleeping_and_stopped_transitions_are_explicit() {
        let mut table = ProcessTable::new();
        let process = table.create_process(None).unwrap();
        let thread = table.create_thread(process, "state-machine").unwrap();

        table.block_thread(thread).unwrap();
        assert_eq!(
            table.thread_snapshot(thread).unwrap().state,
            ThreadState::Blocked
        );
        table.wake_thread(thread).unwrap();
        table.sleep_thread(thread).unwrap();
        table.wake_thread(thread).unwrap();
        table.stop_thread(thread).unwrap();
        table.continue_thread(thread).unwrap();
        assert_eq!(
            table.thread_snapshot(thread).unwrap().state,
            ThreadState::Runnable
        );
        assert_eq!(
            table.yield_thread(thread),
            Err(StateError::InvalidTransition)
        );
    }

    #[test]
    fn detached_threads_are_reclaimed_without_join() {
        let mut table = ProcessTable::new();
        let process = table.create_process(None).unwrap();
        let thread = table.create_thread(process, "detached").unwrap();
        table.detach_thread(thread).unwrap();
        table.exit_thread(thread, 0).unwrap();

        assert_eq!(table.thread_count(), 0);
        assert_eq!(table.process_snapshot(process).unwrap().thread_count, 0);
        assert_eq!(table.join_thread(thread), Err(JoinError::InvalidThread));
    }

    #[test]
    fn exited_process_cannot_accept_new_children_or_threads() {
        let mut table = ProcessTable::new();
        let process = table.create_process(None).unwrap();
        let thread = table.create_thread(process, "last-thread").unwrap();
        table.exit_thread(thread, 1).unwrap();
        table.join_thread(thread).unwrap();

        assert_eq!(
            table.create_thread(process, "late-thread"),
            Err(CreateError::ProcessExited)
        );
        assert_eq!(
            table.create_process(Some(process)),
            Err(CreateError::ProcessExited)
        );
        assert_eq!(table.reap_process(process).unwrap().status, 1);
    }
}
