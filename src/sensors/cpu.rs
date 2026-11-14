use sysinfo::System;

#[derive(Debug)]
pub struct CpuSnapshot {
    pub name: String,
    pub usage_pct: f32,
    pub per_core_mhz: Vec<u64>,
    pub per_core_usage: Vec<f32>,
    pub ram_used_mib: u64,
    pub ram_total_mib: u64,
}

/// Reads CPU usage, per-core clocks, and RAM usage. Infallible: sysinfo
/// returns zeros rather than errors when something can't be read.
pub fn read() -> CpuSnapshot {
    let mut sys = System::new();
    sys.refresh_memory();
    // CPU usage is a delta between two reads, so sample twice with the
    // minimum interval sysinfo requires between them.
    sys.refresh_cpu_all();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_all();

    let name = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_else(|| "unknown CPU".to_string());

    CpuSnapshot {
        name,
        usage_pct: sys.global_cpu_usage(),
        per_core_mhz: sys.cpus().iter().map(|c| c.frequency()).collect(),
        per_core_usage: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
        ram_used_mib: sys.used_memory() / (1024 * 1024),
        ram_total_mib: sys.total_memory() / (1024 * 1024),
    }
}
