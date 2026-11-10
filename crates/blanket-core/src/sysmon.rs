//! System and process resource sampling used by the Memory Manager and the
//! performance panel.

use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tracing::warn;

use crate::memory::SystemMemory;

/// Resource usage of the browser process.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessUsage {
    /// Resident set size / working set in bytes.
    pub resident_bytes: u64,
    /// CPU usage since the previous sample, in percent of one core.
    pub cpu_percent: f32,
}

/// Samples system memory and this process' usage. Only the browser's own process is
/// refreshed, which keeps a sample cheap (no enumeration of every process).
///
/// If the platform refuses to provide statistics the monitor degrades to reporting
/// nothing, and the Memory Manager then simply never detects pressure.
pub struct SystemMonitor {
    system: Option<System>,
    pid: Option<Pid>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
        let refresh = RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram());
        let system = System::new_with_specifics(refresh)
            .inspect_err(|error| warn!(%error, "system statistics unavailable"))
            .ok();
        Self {
            system,
            pid: sysinfo::get_current_pid().ok(),
        }
    }

    pub fn memory(&mut self) -> Option<SystemMemory> {
        let system = self.system.as_mut()?;
        system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        Some(SystemMemory {
            total_bytes: system.total_memory(),
            available_bytes: system.available_memory(),
        })
    }

    /// Usage of the current process. CPU usage needs two samples to be meaningful; the
    /// first call reports 0%.
    pub fn process(&mut self) -> Option<ProcessUsage> {
        let pid = self.pid?;
        let system = self.system.as_mut()?;
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        system.process(pid).map(|process| ProcessUsage {
            resident_bytes: process.memory(),
            cpu_percent: process.cpu_usage(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_plausible() {
        let mut monitor = SystemMonitor::new();
        let memory = monitor.memory().expect("memory statistics");
        assert!(memory.total_bytes > 0);
        assert!(memory.available_bytes <= memory.total_bytes);
        let usage = monitor.process().expect("process statistics");
        assert!(usage.resident_bytes > 0);
    }
}
