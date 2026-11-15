//! Which programs were busy while the hardware struggled. Only the
//! process list, which Windows shows to any user: never the contents of
//! another process.

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// One process as it looked during a sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcSample {
    pub name: String,
    /// Share of the whole CPU, the way Task Manager counts it.
    pub cpu_pct: f32,
    pub ram_mib: u64,
}

pub struct ProcessWatch {
    sys: System,
    cores: f32,
}

impl ProcessWatch {
    /// Only CPU and memory are asked for. Refreshing everything sysinfo
    /// can report about every process costs a few percent of a core each
    /// second, which is more than the rest of heatlog put together.
    fn what_to_read() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::Never)
    }

    pub fn new() -> Self {
        let mut sys = System::new();
        // First refresh only establishes a baseline: per-process CPU is a
        // delta between two readings, like the system-wide figure.
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, Self::what_to_read());
        Self {
            cores: sys.cpus().len().max(1) as f32,
            sys,
        }
    }

    /// The `n` busiest processes by CPU. Processes using no CPU and little
    /// memory are left out: a slowdown is never explained by an idle one.
    pub fn top(&mut self, n: usize) -> Vec<ProcSample> {
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, Self::what_to_read());
        let mut procs: Vec<ProcSample> = self
            .sys
            .processes()
            .values()
            .map(|process| ProcSample {
                name: process.name().to_string_lossy().into_owned(),
                cpu_pct: process.cpu_usage() / self.cores,
                ram_mib: process.memory() / (1024 * 1024),
            })
            .filter(|p| p.cpu_pct >= 0.5 || p.ram_mib >= 500)
            .collect();
        procs.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct));
        procs.truncate(n);
        procs
    }
}

impl Default for ProcessWatch {
    fn default() -> Self {
        Self::new()
    }
}
