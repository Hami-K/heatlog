use std::time::Duration;

use serde::{Deserialize, Serialize};
use sysinfo::System;

use super::cpu_freq::FreqReader;

/// Minimum gap between creating a `CpuSensor` (or a previous read) and the
/// next read: usage and performance counters are deltas between two samples,
/// and short gaps make them meaningless.
pub const MIN_SAMPLE_GAP: Duration = Duration::from_secs(1);

pub struct CpuSensor {
    sys: System,
    freq: Option<FreqReader>,
}

/// One reading; also the on-disk sample format.
#[derive(Debug, Serialize, Deserialize)]
pub struct CpuSample {
    pub usage_pct: f32,
    pub per_core_mhz: Vec<u64>,
    pub per_core_usage: Vec<f32>,
}

#[derive(Debug)]
pub struct CpuSnapshot {
    pub name: String,
    pub sample: CpuSample,
    /// False when live performance counters were unavailable and
    /// `per_core_mhz` holds the static base clock instead.
    pub live_clocks: bool,
    pub ram_used_mib: u64,
    pub ram_total_mib: u64,
}

impl CpuSensor {
    /// Takes a first (discarded) sample of the delta-based counters so the
    /// next `read` has a baseline. Live clocks fall back to the static base
    /// clock if the performance counters can't be opened.
    // No Default impl: constructing a sensor opens WMI handles and takes a
    // baseline sample, which a "default value" should not silently do.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let freq = FreqReader::new().ok();
        if let Some(f) = &freq {
            let _ = f.read_mhz();
        }
        let mut sys = System::new();
        sys.refresh_cpu_all();
        Self { sys, freq }
    }

    pub fn read(&mut self) -> CpuSnapshot {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();

        let live_mhz = self.freq.as_ref().and_then(|f| f.read_mhz().ok());
        let live_clocks = live_mhz.is_some();
        let per_core_mhz =
            live_mhz.unwrap_or_else(|| self.sys.cpus().iter().map(|c| c.frequency()).collect());

        CpuSnapshot {
            name: self
                .sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string())
                .unwrap_or_else(|| "unknown CPU".to_string()),
            sample: CpuSample {
                usage_pct: self.sys.global_cpu_usage(),
                per_core_mhz,
                per_core_usage: self.sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
            },
            live_clocks,
            ram_used_mib: self.sys.used_memory() / (1024 * 1024),
            ram_total_mib: self.sys.total_memory() / (1024 * 1024),
        }
    }
}
