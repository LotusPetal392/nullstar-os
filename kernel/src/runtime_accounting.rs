//! Runtime CPU and memory accounting for the execution foundation.
//!
//! The scheduler owns execution state and the memory subsystem owns mappings;
//! this module owns the bounded counters that connect those systems to
//! process/thread/job observability.  It is deliberately architecture-neutral
//! so the same accounting rules can be exercised by host tests before they are
//! driven by real per-CPU scheduler events.

use alloc::vec::Vec;

use crate::{process_model::{ProcessId, ThreadId}, scheduling::CpuId};

pub const MAX_ACCOUNTED_PROCESSES: usize = 64;
pub const MAX_ACCOUNTED_THREADS: usize = 128;
pub const MAX_ACCOUNTED_CPUS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuMode {
    User,
    Kernel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountingError {
    ProcessLimitReached,
    ThreadLimitReached,
    CpuLimitReached,
    InvalidProcess,
    InvalidThread,
    InvalidCpu,
    DuplicateProcess,
    DuplicateThread,
    DuplicateCpu,
    MemoryUnderflow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThreadRuntime {
    pub thread_id: ThreadId,
    pub process_id: ProcessId,
    pub cpu_ticks: u64,
    pub user_ticks: u64,
    pub kernel_ticks: u64,
    pub preemptions: u64,
    pub voluntary_switches: u64,
    pub migrations: u64,
    pub last_cpu: Option<CpuId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProcessRuntime {
    pub process_id: ProcessId,
    pub cpu_ticks: u64,
    pub user_ticks: u64,
    pub kernel_ticks: u64,
    pub memory_bytes: u64,
    pub peak_memory_bytes: u64,
    pub mapped_bytes: u64,
    pub peak_mapped_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuRuntime {
    pub cpu: CpuId,
    pub busy_ticks: u64,
    pub idle_ticks: u64,
    pub context_switches: u64,
    pub preemptions: u64,
    pub voluntary_switches: u64,
    pub migrations_in: u64,
    pub migrations_out: u64,
}

impl CpuRuntime {
    pub const fn utilization_basis_points(self) -> u16 {
        let total = self.busy_ticks.saturating_add(self.idle_ticks);
        if total == 0 {
            return 0;
        }
        let ratio = self.busy_ticks.saturating_mul(10_000) / total;
        if ratio > 10_000 { 10_000 } else { ratio as u16 }
    }
}

/// Bounded execution accounting registry.
///
/// A scheduler tick should call [`RuntimeAccounting::account_tick`] for the
/// currently running thread.  Context-switch and migration paths should call
/// the corresponding event methods.  Memory ownership changes should charge
/// and release bytes at the process boundary.  All counters saturate rather
/// than wrapping, making diagnostics deterministic even if a long-lived
/// system eventually exhausts a counter.
pub struct RuntimeAccounting {
    processes: Vec<ProcessRuntime>,
    threads: Vec<ThreadRuntime>,
    cpus: Vec<CpuRuntime>,
}

impl Default for RuntimeAccounting {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeAccounting {
    pub const fn new() -> Self {
        Self {
            processes: Vec::new(),
            threads: Vec::new(),
            cpus: Vec::new(),
        }
    }

    pub fn register_process(&mut self, process_id: ProcessId) -> Result<(), AccountingError> {
        if self.processes.iter().any(|process| process.process_id == process_id) {
            return Err(AccountingError::DuplicateProcess);
        }
        if self.processes.len() >= MAX_ACCOUNTED_PROCESSES {
            return Err(AccountingError::ProcessLimitReached);
        }
        self.processes.push(ProcessRuntime {
            process_id,
            ..ProcessRuntime::default()
        });
        Ok(())
    }

    pub fn unregister_process(&mut self, process_id: ProcessId) -> Result<ProcessRuntime, AccountingError> {
        let index = self
            .processes
            .iter()
            .position(|process| process.process_id == process_id)
            .ok_or(AccountingError::InvalidProcess)?;
        if self.threads.iter().any(|thread| thread.process_id == process_id) {
            return Err(AccountingError::InvalidThread);
        }
        Ok(self.processes.swap_remove(index))
    }

    pub fn register_thread(
        &mut self,
        thread_id: ThreadId,
        process_id: ProcessId,
    ) -> Result<(), AccountingError> {
        if !self.processes.iter().any(|process| process.process_id == process_id) {
            return Err(AccountingError::InvalidProcess);
        }
        if self.threads.iter().any(|thread| thread.thread_id == thread_id) {
            return Err(AccountingError::DuplicateThread);
        }
        if self.threads.len() >= MAX_ACCOUNTED_THREADS {
            return Err(AccountingError::ThreadLimitReached);
        }
        self.threads.push(ThreadRuntime {
            thread_id,
            process_id,
            ..ThreadRuntime::default()
        });
        Ok(())
    }

    pub fn unregister_thread(&mut self, thread_id: ThreadId) -> Result<ThreadRuntime, AccountingError> {
        let index = self
            .threads
            .iter()
            .position(|thread| thread.thread_id == thread_id)
            .ok_or(AccountingError::InvalidThread)?;
        Ok(self.threads.swap_remove(index))
    }

    pub fn register_cpu(&mut self, cpu: CpuId) -> Result<(), AccountingError> {
        if self.cpus.iter().any(|record| record.cpu == cpu) {
            return Err(AccountingError::DuplicateCpu);
        }
        if self.cpus.len() >= MAX_ACCOUNTED_CPUS {
            return Err(AccountingError::CpuLimitReached);
        }
        self.cpus.push(CpuRuntime {
            cpu,
            ..CpuRuntime::default()
        });
        Ok(())
    }

    pub fn account_tick(
        &mut self,
        thread_id: ThreadId,
        cpu: CpuId,
        mode: CpuMode,
    ) -> Result<(), AccountingError> {
        let thread_index = self.thread_index(thread_id)?;
        let process_id = self.threads[thread_index].process_id;
        let process_index = self.process_index(process_id)?;
        let cpu_index = self.cpu_index(cpu)?;

        let thread = &mut self.threads[thread_index];
        thread.cpu_ticks = thread.cpu_ticks.saturating_add(1);
        match mode {
            CpuMode::User => thread.user_ticks = thread.user_ticks.saturating_add(1),
            CpuMode::Kernel => thread.kernel_ticks = thread.kernel_ticks.saturating_add(1),
        }
        thread.last_cpu = Some(cpu);

        let process = &mut self.processes[process_index];
        process.cpu_ticks = process.cpu_ticks.saturating_add(1);
        match mode {
            CpuMode::User => process.user_ticks = process.user_ticks.saturating_add(1),
            CpuMode::Kernel => process.kernel_ticks = process.kernel_ticks.saturating_add(1),
        }

        self.cpus[cpu_index].busy_ticks = self.cpus[cpu_index].busy_ticks.saturating_add(1);
        Ok(())
    }

    pub fn account_idle_tick(&mut self, cpu: CpuId) -> Result<(), AccountingError> {
        let cpu_index = self.cpu_index(cpu)?;
        self.cpus[cpu_index].idle_ticks = self.cpus[cpu_index].idle_ticks.saturating_add(1);
        Ok(())
    }

    pub fn account_context_switch(
        &mut self,
        cpu: CpuId,
        thread_id: ThreadId,
        preempted: bool,
    ) -> Result<(), AccountingError> {
        let thread_index = self.thread_index(thread_id)?;
        let cpu_index = self.cpu_index(cpu)?;
        self.threads[thread_index].last_cpu = Some(cpu);
        self.cpus[cpu_index].context_switches =
            self.cpus[cpu_index].context_switches.saturating_add(1);
        if preempted {
            self.threads[thread_index].preemptions =
                self.threads[thread_index].preemptions.saturating_add(1);
            self.cpus[cpu_index].preemptions = self.cpus[cpu_index].preemptions.saturating_add(1);
        } else {
            self.threads[thread_index].voluntary_switches =
                self.threads[thread_index].voluntary_switches.saturating_add(1);
            self.cpus[cpu_index].voluntary_switches =
                self.cpus[cpu_index].voluntary_switches.saturating_add(1);
        }
        Ok(())
    }

    pub fn account_migration(
        &mut self,
        thread_id: ThreadId,
        from: CpuId,
        to: CpuId,
    ) -> Result<(), AccountingError> {
        let thread_index = self.thread_index(thread_id)?;
        let from_index = self.cpu_index(from)?;
        let to_index = self.cpu_index(to)?;
        self.threads[thread_index].migrations =
            self.threads[thread_index].migrations.saturating_add(1);
        self.threads[thread_index].last_cpu = Some(to);
        self.cpus[from_index].migrations_out = self.cpus[from_index].migrations_out.saturating_add(1);
        self.cpus[to_index].migrations_in = self.cpus[to_index].migrations_in.saturating_add(1);
        Ok(())
    }

    pub fn charge_memory(
        &mut self,
        process_id: ProcessId,
        bytes: u64,
    ) -> Result<(), AccountingError> {
        let process = self.process_mut(process_id)?;
        process.memory_bytes = process.memory_bytes.saturating_add(bytes);
        process.peak_memory_bytes = process.peak_memory_bytes.max(process.memory_bytes);
        Ok(())
    }

    pub fn release_memory(
        &mut self,
        process_id: ProcessId,
        bytes: u64,
    ) -> Result<(), AccountingError> {
        let process = self.process_mut(process_id)?;
        if bytes > process.memory_bytes {
            return Err(AccountingError::MemoryUnderflow);
        }
        process.memory_bytes -= bytes;
        Ok(())
    }

    pub fn charge_mapped_memory(
        &mut self,
        process_id: ProcessId,
        bytes: u64,
    ) -> Result<(), AccountingError> {
        let process = self.process_mut(process_id)?;
        process.mapped_bytes = process.mapped_bytes.saturating_add(bytes);
        process.peak_mapped_bytes = process.peak_mapped_bytes.max(process.mapped_bytes);
        Ok(())
    }

    pub fn release_mapped_memory(
        &mut self,
        process_id: ProcessId,
        bytes: u64,
    ) -> Result<(), AccountingError> {
        let process = self.process_mut(process_id)?;
        if bytes > process.mapped_bytes {
            return Err(AccountingError::MemoryUnderflow);
        }
        process.mapped_bytes -= bytes;
        Ok(())
    }

    pub fn process(&self, process_id: ProcessId) -> Result<ProcessRuntime, AccountingError> {
        Ok(self.processes[self.process_index(process_id)?])
    }

    pub fn thread(&self, thread_id: ThreadId) -> Result<ThreadRuntime, AccountingError> {
        Ok(self.threads[self.thread_index(thread_id)?])
    }

    pub fn cpu(&self, cpu: CpuId) -> Result<CpuRuntime, AccountingError> {
        Ok(self.cpus[self.cpu_index(cpu)?])
    }

    pub fn processes(&self) -> &[ProcessRuntime] {
        &self.processes
    }

    pub fn threads(&self) -> &[ThreadRuntime] {
        &self.threads
    }

    pub fn cpus(&self) -> &[CpuRuntime] {
        &self.cpus
    }

    fn process_index(&self, process_id: ProcessId) -> Result<usize, AccountingError> {
        self.processes
            .iter()
            .position(|process| process.process_id == process_id)
            .ok_or(AccountingError::InvalidProcess)
    }

    fn process_mut(&mut self, process_id: ProcessId) -> Result<&mut ProcessRuntime, AccountingError> {
        let index = self.process_index(process_id)?;
        Ok(&mut self.processes[index])
    }

    fn thread_index(&self, thread_id: ThreadId) -> Result<usize, AccountingError> {
        self.threads
            .iter()
            .position(|thread| thread.thread_id == thread_id)
            .ok_or(AccountingError::InvalidThread)
    }

    fn cpu_index(&self, cpu: CpuId) -> Result<usize, AccountingError> {
        self.cpus
            .iter()
            .position(|record| record.cpu == cpu)
            .ok_or(AccountingError::InvalidCpu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(raw: u64) -> ProcessId {
        ProcessId::from_raw(raw).unwrap()
    }

    fn thread(raw: u64) -> ThreadId {
        ThreadId::from_raw(raw).unwrap()
    }

    fn cpu(raw: usize) -> CpuId {
        CpuId::from_raw(raw).unwrap()
    }

    #[test]
    fn accounts_thread_and_process_runtime_by_cpu_and_mode() {
        let mut accounting = RuntimeAccounting::new();
        accounting.register_process(process(1)).unwrap();
        accounting.register_thread(thread(1), process(1)).unwrap();
        accounting.register_cpu(cpu(0)).unwrap();

        accounting.account_tick(thread(1), cpu(0), CpuMode::User).unwrap();
        accounting.account_tick(thread(1), cpu(0), CpuMode::Kernel).unwrap();
        accounting.account_context_switch(cpu(0), thread(1), true).unwrap();

        let thread_runtime = accounting.thread(thread(1)).unwrap();
        assert_eq!(thread_runtime.cpu_ticks, 2);
        assert_eq!(thread_runtime.user_ticks, 1);
        assert_eq!(thread_runtime.kernel_ticks, 1);
        assert_eq!(thread_runtime.preemptions, 1);
        assert_eq!(thread_runtime.last_cpu, Some(cpu(0)));

        let process_runtime = accounting.process(process(1)).unwrap();
        assert_eq!(process_runtime.cpu_ticks, 2);
        assert_eq!(process_runtime.user_ticks, 1);
        assert_eq!(process_runtime.kernel_ticks, 1);

        let cpu_runtime = accounting.cpu(cpu(0)).unwrap();
        assert_eq!(cpu_runtime.busy_ticks, 2);
        assert_eq!(cpu_runtime.context_switches, 1);
        assert_eq!(cpu_runtime.preemptions, 1);
        assert_eq!(cpu_runtime.utilization_basis_points(), 10_000);
    }

    #[test]
    fn tracks_idle_time_and_migrations() {
        let mut accounting = RuntimeAccounting::new();
        accounting.register_process(process(1)).unwrap();
        accounting.register_thread(thread(1), process(1)).unwrap();
        accounting.register_cpu(cpu(0)).unwrap();
        accounting.register_cpu(cpu(1)).unwrap();

        accounting.account_idle_tick(cpu(0)).unwrap();
        accounting.account_tick(thread(1), cpu(0), CpuMode::User).unwrap();
        accounting.account_migration(thread(1), cpu(0), cpu(1)).unwrap();
        accounting.account_tick(thread(1), cpu(1), CpuMode::User).unwrap();

        assert_eq!(accounting.cpu(cpu(0)).unwrap().idle_ticks, 1);
        assert_eq!(accounting.cpu(cpu(0)).unwrap().migrations_out, 1);
        assert_eq!(accounting.cpu(cpu(1)).unwrap().migrations_in, 1);
        assert_eq!(accounting.thread(thread(1)).unwrap().migrations, 1);
        assert_eq!(accounting.thread(thread(1)).unwrap().last_cpu, Some(cpu(1)));
    }

    #[test]
    fn tracks_current_and_peak_memory_and_rejects_underflow() {
        let mut accounting = RuntimeAccounting::new();
        accounting.register_process(process(1)).unwrap();

        accounting.charge_memory(process(1), 4096).unwrap();
        accounting.charge_memory(process(1), 4096).unwrap();
        accounting.charge_mapped_memory(process(1), 4096).unwrap();
        accounting.release_memory(process(1), 2048).unwrap();
        accounting.release_mapped_memory(process(1), 1024).unwrap();

        let runtime = accounting.process(process(1)).unwrap();
        assert_eq!(runtime.memory_bytes, 6144);
        assert_eq!(runtime.peak_memory_bytes, 8192);
        assert_eq!(runtime.mapped_bytes, 3072);
        assert_eq!(runtime.peak_mapped_bytes, 4096);
        assert_eq!(
            accounting.release_memory(process(1), 8192),
            Err(AccountingError::MemoryUnderflow)
        );
    }

    #[test]
    fn unregister_requires_threads_to_be_reclaimed_first() {
        let mut accounting = RuntimeAccounting::new();
        accounting.register_process(process(1)).unwrap();
        accounting.register_thread(thread(1), process(1)).unwrap();

        assert_eq!(
            accounting.unregister_process(process(1)),
            Err(AccountingError::InvalidThread)
        );
        accounting.unregister_thread(thread(1)).unwrap();
        assert_eq!(accounting.unregister_process(process(1)).unwrap().process_id, process(1));
    }
}
