//! Runtime CPU and memory accounting for the execution foundation.
//!
//! This architecture-neutral layer defines the counters that connect the live
//! scheduler and memory subsystem to process/thread/job observability.  It is
//! intentionally host-testable before the counters are driven by real per-CPU
//! events.

use alloc::vec::Vec;

use crate::{process_model::{ProcessId, ThreadId}, scheduling::CpuId};

pub const MAX_ACCOUNTED_PROCESSES: usize = 64;
pub const MAX_ACCOUNTED_THREADS: usize = 128;
pub const MAX_ACCOUNTED_CPUS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuMode { User, Kernel }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountingError {
    ProcessLimitReached, ThreadLimitReached, CpuLimitReached,
    InvalidProcess, InvalidThread, InvalidCpu,
    DuplicateProcess, DuplicateThread, DuplicateCpu, MemoryUnderflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThreadRuntime {
    pub thread_id: ThreadId, pub process_id: ProcessId,
    pub cpu_ticks: u64, pub user_ticks: u64, pub kernel_ticks: u64,
    pub preemptions: u64, pub voluntary_switches: u64, pub migrations: u64,
    pub last_cpu: Option<CpuId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessRuntime {
    pub process_id: ProcessId,
    pub cpu_ticks: u64, pub user_ticks: u64, pub kernel_ticks: u64,
    pub memory_bytes: u64, pub peak_memory_bytes: u64,
    pub mapped_bytes: u64, pub peak_mapped_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuRuntime {
    pub cpu: CpuId,
    pub busy_ticks: u64, pub idle_ticks: u64,
    pub context_switches: u64, pub preemptions: u64, pub voluntary_switches: u64,
    pub migrations_in: u64, pub migrations_out: u64,
}

impl CpuRuntime {
    pub const fn utilization_basis_points(self) -> u16 {
        let total = self.busy_ticks.saturating_add(self.idle_ticks);
        if total == 0 { return 0; }
        let ratio = self.busy_ticks.saturating_mul(10_000) / total;
        if ratio > 10_000 { 10_000 } else { ratio as u16 }
    }
}

pub struct RuntimeAccounting {
    processes: Vec<ProcessRuntime>,
    threads: Vec<ThreadRuntime>,
    cpus: Vec<CpuRuntime>,
}

impl Default for RuntimeAccounting { fn default() -> Self { Self::new() } }

impl RuntimeAccounting {
    pub const fn new() -> Self {
        Self { processes: Vec::new(), threads: Vec::new(), cpus: Vec::new() }
    }

    pub fn register_process(&mut self, process_id: ProcessId) -> Result<(), AccountingError> {
        if self.processes.iter().any(|p| p.process_id == process_id) { return Err(AccountingError::DuplicateProcess); }
        if self.processes.len() >= MAX_ACCOUNTED_PROCESSES { return Err(AccountingError::ProcessLimitReached); }
        self.processes.push(ProcessRuntime { process_id, cpu_ticks: 0, user_ticks: 0, kernel_ticks: 0, memory_bytes: 0, peak_memory_bytes: 0, mapped_bytes: 0, peak_mapped_bytes: 0 });
        Ok(())
    }

    pub fn unregister_process(&mut self, process_id: ProcessId) -> Result<ProcessRuntime, AccountingError> {
        let index = self.process_index(process_id)?;
        if self.threads.iter().any(|t| t.process_id == process_id) { return Err(AccountingError::InvalidThread); }
        Ok(self.processes.swap_remove(index))
    }

    pub fn register_thread(&mut self, thread_id: ThreadId, process_id: ProcessId) -> Result<(), AccountingError> {
        self.process_index(process_id)?;
        if self.threads.iter().any(|t| t.thread_id == thread_id) { return Err(AccountingError::DuplicateThread); }
        if self.threads.len() >= MAX_ACCOUNTED_THREADS { return Err(AccountingError::ThreadLimitReached); }
        self.threads.push(ThreadRuntime { thread_id, process_id, cpu_ticks: 0, user_ticks: 0, kernel_ticks: 0, preemptions: 0, voluntary_switches: 0, migrations: 0, last_cpu: None });
        Ok(())
    }

    pub fn unregister_thread(&mut self, thread_id: ThreadId) -> Result<ThreadRuntime, AccountingError> {
        let index = self.thread_index(thread_id)?;
        Ok(self.threads.swap_remove(index))
    }

    pub fn register_cpu(&mut self, cpu: CpuId) -> Result<(), AccountingError> {
        if self.cpus.iter().any(|c| c.cpu == cpu) { return Err(AccountingError::DuplicateCpu); }
        if self.cpus.len() >= MAX_ACCOUNTED_CPUS { return Err(AccountingError::CpuLimitReached); }
        self.cpus.push(CpuRuntime { cpu, busy_ticks: 0, idle_ticks: 0, context_switches: 0, preemptions: 0, voluntary_switches: 0, migrations_in: 0, migrations_out: 0 });
        Ok(())
    }

    pub fn account_tick(&mut self, thread_id: ThreadId, cpu: CpuId, mode: CpuMode) -> Result<(), AccountingError> {
        let ti = self.thread_index(thread_id)?;
        let pi = self.process_index(self.threads[ti].process_id)?;
        let ci = self.cpu_index(cpu)?;
        self.threads[ti].cpu_ticks = self.threads[ti].cpu_ticks.saturating_add(1);
        self.threads[ti].last_cpu = Some(cpu);
        self.processes[pi].cpu_ticks = self.processes[pi].cpu_ticks.saturating_add(1);
        self.cpus[ci].busy_ticks = self.cpus[ci].busy_ticks.saturating_add(1);
        match mode {
            CpuMode::User => { self.threads[ti].user_ticks = self.threads[ti].user_ticks.saturating_add(1); self.processes[pi].user_ticks = self.processes[pi].user_ticks.saturating_add(1); }
            CpuMode::Kernel => { self.threads[ti].kernel_ticks = self.threads[ti].kernel_ticks.saturating_add(1); self.processes[pi].kernel_ticks = self.processes[pi].kernel_ticks.saturating_add(1); }
        }
        Ok(())
    }

    pub fn account_idle_tick(&mut self, cpu: CpuId) -> Result<(), AccountingError> {
        let ci = self.cpu_index(cpu)?;
        self.cpus[ci].idle_ticks = self.cpus[ci].idle_ticks.saturating_add(1);
        Ok(())
    }

    pub fn account_context_switch(&mut self, cpu: CpuId, thread_id: ThreadId, preempted: bool) -> Result<(), AccountingError> {
        let ti = self.thread_index(thread_id)?;
        let ci = self.cpu_index(cpu)?;
        self.threads[ti].last_cpu = Some(cpu);
        self.cpus[ci].context_switches = self.cpus[ci].context_switches.saturating_add(1);
        if preempted { self.threads[ti].preemptions = self.threads[ti].preemptions.saturating_add(1); self.cpus[ci].preemptions = self.cpus[ci].preemptions.saturating_add(1); }
        else { self.threads[ti].voluntary_switches = self.threads[ti].voluntary_switches.saturating_add(1); self.cpus[ci].voluntary_switches = self.cpus[ci].voluntary_switches.saturating_add(1); }
        Ok(())
    }

    pub fn account_migration(&mut self, thread_id: ThreadId, from: CpuId, to: CpuId) -> Result<(), AccountingError> {
        let ti = self.thread_index(thread_id)?;
        let fi = self.cpu_index(from)?;
        let ti_cpu = self.cpu_index(to)?;
        self.threads[ti].migrations = self.threads[ti].migrations.saturating_add(1);
        self.threads[ti].last_cpu = Some(to);
        self.cpus[fi].migrations_out = self.cpus[fi].migrations_out.saturating_add(1);
        self.cpus[ti_cpu].migrations_in = self.cpus[ti_cpu].migrations_in.saturating_add(1);
        Ok(())
    }

    pub fn charge_memory(&mut self, process_id: ProcessId, bytes: u64) -> Result<(), AccountingError> {
        let p = self.process_mut(process_id)?;
        p.memory_bytes = p.memory_bytes.saturating_add(bytes);
        p.peak_memory_bytes = p.peak_memory_bytes.max(p.memory_bytes);
        Ok(())
    }

    pub fn release_memory(&mut self, process_id: ProcessId, bytes: u64) -> Result<(), AccountingError> {
        let p = self.process_mut(process_id)?;
        if bytes > p.memory_bytes { return Err(AccountingError::MemoryUnderflow); }
        p.memory_bytes -= bytes;
        Ok(())
    }

    pub fn charge_mapped_memory(&mut self, process_id: ProcessId, bytes: u64) -> Result<(), AccountingError> {
        let p = self.process_mut(process_id)?;
        p.mapped_bytes = p.mapped_bytes.saturating_add(bytes);
        p.peak_mapped_bytes = p.peak_mapped_bytes.max(p.mapped_bytes);
        Ok(())
    }

    pub fn release_mapped_memory(&mut self, process_id: ProcessId, bytes: u64) -> Result<(), AccountingError> {
        let p = self.process_mut(process_id)?;
        if bytes > p.mapped_bytes { return Err(AccountingError::MemoryUnderflow); }
        p.mapped_bytes -= bytes;
        Ok(())
    }

    pub fn process(&self, id: ProcessId) -> Result<ProcessRuntime, AccountingError> { Ok(self.processes[self.process_index(id)?]) }
    pub fn thread(&self, id: ThreadId) -> Result<ThreadRuntime, AccountingError> { Ok(self.threads[self.thread_index(id)?]) }
    pub fn cpu(&self, id: CpuId) -> Result<CpuRuntime, AccountingError> { Ok(self.cpus[self.cpu_index(id)?]) }
    pub fn processes(&self) -> &[ProcessRuntime] { &self.processes }
    pub fn threads(&self) -> &[ThreadRuntime] { &self.threads }
    pub fn cpus(&self) -> &[CpuRuntime] { &self.cpus }

    fn process_index(&self, id: ProcessId) -> Result<usize, AccountingError> { self.processes.iter().position(|p| p.process_id == id).ok_or(AccountingError::InvalidProcess) }
    fn process_mut(&mut self, id: ProcessId) -> Result<&mut ProcessRuntime, AccountingError> { let i = self.process_index(id)?; Ok(&mut self.processes[i]) }
    fn thread_index(&self, id: ThreadId) -> Result<usize, AccountingError> { self.threads.iter().position(|t| t.thread_id == id).ok_or(AccountingError::InvalidThread) }
    fn cpu_index(&self, id: CpuId) -> Result<usize, AccountingError> { self.cpus.iter().position(|c| c.cpu == id).ok_or(AccountingError::InvalidCpu) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pid(v: u64) -> ProcessId { ProcessId::from_raw(v).unwrap() }
    fn tid(v: u64) -> ThreadId { ThreadId::from_raw(v).unwrap() }
    fn cpu(v: usize) -> CpuId { CpuId::from_raw(v).unwrap() }

    #[test]
    fn aggregates_thread_process_and_cpu_runtime() {
        let mut a = RuntimeAccounting::new();
        a.register_process(pid(1)).unwrap(); a.register_thread(tid(1), pid(1)).unwrap(); a.register_cpu(cpu(0)).unwrap();
        a.account_tick(tid(1), cpu(0), CpuMode::User).unwrap(); a.account_tick(tid(1), cpu(0), CpuMode::Kernel).unwrap();
        a.account_context_switch(cpu(0), tid(1), true).unwrap();
        assert_eq!(a.thread(tid(1)).unwrap().user_ticks, 1); assert_eq!(a.thread(tid(1)).unwrap().kernel_ticks, 1);
        assert_eq!(a.process(pid(1)).unwrap().cpu_ticks, 2); assert_eq!(a.cpu(cpu(0)).unwrap().busy_ticks, 2);
        assert_eq!(a.cpu(cpu(0)).unwrap().preemptions, 1);
    }

    #[test]
    fn accounts_idle_time_and_migration() {
        let mut a = RuntimeAccounting::new();
        a.register_process(pid(1)).unwrap(); a.register_thread(tid(1), pid(1)).unwrap(); a.register_cpu(cpu(0)).unwrap(); a.register_cpu(cpu(1)).unwrap();
        a.account_idle_tick(cpu(0)).unwrap(); a.account_migration(tid(1), cpu(0), cpu(1)).unwrap();
        assert_eq!(a.cpu(cpu(0)).unwrap().idle_ticks, 1); assert_eq!(a.cpu(cpu(0)).unwrap().migrations_out, 1); assert_eq!(a.cpu(cpu(1)).unwrap().migrations_in, 1);
    }

    #[test]
    fn tracks_memory_and_peak_without_underflow() {
        let mut a = RuntimeAccounting::new(); a.register_process(pid(1)).unwrap();
        a.charge_memory(pid(1), 4096).unwrap(); a.charge_memory(pid(1), 4096).unwrap(); a.release_memory(pid(1), 2048).unwrap();
        a.charge_mapped_memory(pid(1), 4096).unwrap(); a.release_mapped_memory(pid(1), 1024).unwrap();
        let p = a.process(pid(1)).unwrap(); assert_eq!(p.memory_bytes, 6144); assert_eq!(p.peak_memory_bytes, 8192); assert_eq!(p.mapped_bytes, 3072);
        assert_eq!(a.release_memory(pid(1), 8192), Err(AccountingError::MemoryUnderflow));
    }

    #[test]
    fn process_reclamation_waits_for_threads() {
        let mut a = RuntimeAccounting::new(); a.register_process(pid(1)).unwrap(); a.register_thread(tid(1), pid(1)).unwrap();
        assert_eq!(a.unregister_process(pid(1)), Err(AccountingError::InvalidThread)); a.unregister_thread(tid(1)).unwrap();
        assert_eq!(a.unregister_process(pid(1)).unwrap().process_id, pid(1));
    }
}
