//! Recording engine shared by frontends: sensor setup, per-tick sampling,
//! session writing and game watching. Frontends own the loop, the timing
//! and all display.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use chrono::{Local, Utc};
use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::sensors::cpu::CpuSensor;
use crate::sensors::cpu_temp::{self, CpuTempSource};
use crate::sensors::gpu::{GpuInfo, GpuSensor};
use crate::sensors::processes::ProcessWatch;
use crate::session::{Sample, SessionMeta, SessionWriter};

/// All sensors behind one handle, usable for live monitoring without
/// recording anything. Create and use it on one thread: the WMI handles
/// stay on it.
pub struct Sensors {
    gpu: Option<GpuSensor>,
    gpu_info: Option<GpuInfo>,
    cpu: CpuSensor,
    temp_source: Option<Box<dyn CpuTempSource>>,
    /// None when the user has turned process recording off.
    processes: Option<ProcessWatch>,
    cpu_name: String,
    ram_total_mib: u64,
}

impl Sensors {
    /// The first init on a machine can take a few seconds (WMI/COM
    /// startup, temperature source probing).
    pub fn init(lhm_url: &str, record_processes: bool) -> Self {
        let gpu = GpuSensor::new().ok();
        let gpu_info = gpu.as_ref().and_then(|g| g.info().ok());
        let mut cpu = CpuSensor::new();
        let temp_source = cpu_temp::detect(lhm_url);
        let first_read = cpu.read();
        Self {
            gpu,
            gpu_info,
            cpu,
            temp_source,
            processes: record_processes.then(ProcessWatch::new),
            cpu_name: first_read.name,
            ram_total_mib: first_read.ram_total_mib,
        }
    }

    pub fn gpu_available(&self) -> bool {
        self.gpu.is_some()
    }

    pub fn gpu_info(&self) -> Option<&GpuInfo> {
        self.gpu_info.as_ref()
    }

    pub fn cpu_name(&self) -> &str {
        &self.cpu_name
    }

    pub fn ram_total_mib(&self) -> u64 {
        self.ram_total_mib
    }

    pub fn cpu_temp_source(&self) -> Option<&'static str> {
        self.temp_source.as_ref().map(|s| s.label())
    }

    /// Looks for a temperature source again, after the user has started or
    /// configured one.
    pub fn reprobe_temp(&mut self, lhm_url: &str) {
        self.temp_source = cpu_temp::detect(lhm_url);
    }

    /// Reads all sensors once. Call at the sampling interval; shorter gaps
    /// make the delta-based counters meaningless.
    pub fn read_sample(&mut self) -> Sample {
        let cpu_snap = self.cpu.read();
        let gpu_snap = self.gpu.as_ref().and_then(|g| g.read().ok());
        let reading = self
            .temp_source
            .as_mut()
            .and_then(|source| source.read().ok())
            .unwrap_or_default();
        let top_procs = self
            .processes
            .as_mut()
            .map(|watch| watch.top(5))
            .unwrap_or_default();
        Sample::new(Utc::now(), cpu_snap, gpu_snap, reading, top_procs)
    }

    pub fn session_meta(&self, interval_secs: u64, game: Option<String>) -> SessionMeta {
        SessionMeta {
            started: Local::now(),
            interval_secs,
            cpu_name: self.cpu_name.clone(),
            ram_total_mib: Some(self.ram_total_mib),
            gpu_name: self.gpu_info.as_ref().map(|g| g.name.clone()),
            vram_total_mib: self.gpu_info.as_ref().map(|g| g.vram_total_mib),
            cpu_temp_source: self.cpu_temp_source().map(str::to_string),
            game,
        }
    }
}

pub struct Recorder {
    sensors: Sensors,
    writer: SessionWriter,
    game: Option<(Pid, String)>,
    process_watch: System,
    count: u64,
}

pub struct Step {
    pub sample: Sample,
    /// The watched game's name when it has exited; stop after this step.
    pub game_exited: Option<String>,
}

impl Recorder {
    pub fn start(
        lhm_url: &str,
        sessions_dir: &Path,
        interval_secs: u64,
        game: Option<(Pid, String)>,
        record_processes: bool,
    ) -> Result<Self> {
        let sensors = Sensors::init(lhm_url, record_processes);
        let meta = sensors.session_meta(interval_secs, game.as_ref().map(|(_, n)| n.clone()));
        let writer = SessionWriter::create(sessions_dir, &meta)?;
        Ok(Self {
            sensors,
            writer,
            game,
            process_watch: System::new(),
            count: 0,
        })
    }

    pub fn gpu_available(&self) -> bool {
        self.sensors.gpu_available()
    }

    pub fn cpu_temp_source(&self) -> Option<&'static str> {
        self.sensors.cpu_temp_source()
    }

    pub fn path(&self) -> &Path {
        self.writer.path()
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Reads all sensors once, appends the sample to the session file, and
    /// checks whether the watched game is still running.
    pub fn step(&mut self) -> Result<Step> {
        let sample = self.sensors.read_sample();
        self.writer.append(&sample)?;
        self.count += 1;

        let game_exited = match &self.game {
            Some((pid, name)) => {
                self.process_watch
                    .refresh_processes(ProcessesToUpdate::All, true);
                self.process_watch
                    .process(*pid)
                    .is_none()
                    .then(|| name.clone())
            }
            None => None,
        };
        Ok(Step {
            sample,
            game_exited,
        })
    }
}

/// One pass over the process list: the first process whose name matches one
/// of `games`, case-insensitively.
pub fn scan_for_game(sys: &mut System, games: &[String]) -> Option<(Pid, String)> {
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().values().find_map(|process| {
        let name = process.name().to_string_lossy();
        games
            .iter()
            .any(|g| g.eq_ignore_ascii_case(&name))
            .then(|| (process.pid(), name.into_owned()))
    })
}

/// Polls the process list every two seconds until a matching process
/// appears, or the stop flag is set (returns None).
pub fn wait_for_game(games: &[String], stop: &AtomicBool) -> Option<(Pid, String)> {
    let mut sys = System::new();
    while !stop.load(Ordering::SeqCst) {
        if let Some(found) = scan_for_game(&mut sys, games) {
            return Some(found);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: start, one sample, read the file back. Runs on any
    /// Windows machine or CI runner — no GPU needed (the GPU path degrades
    /// to None) and the LHM URL points at a closed port.
    #[test]
    fn records_meta_and_one_sample() {
        let dir = std::env::temp_dir().join(format!("heatlog-rec-test-{}", std::process::id()));
        let mut rec = Recorder::start("http://127.0.0.1:9/none", &dir, 1, None, true).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let step = rec.step().unwrap();
        assert!(step.game_exited.is_none());
        assert_eq!(rec.count(), 1);

        let session = crate::session::read(rec.path()).unwrap();
        assert_eq!(session.meta.interval_secs, 1);
        assert_eq!(session.samples.len(), 1);
        assert!(!session.samples[0].cpu.per_core_mhz.is_empty());
        // Process recording was on, and something is always running.
        assert!(!session.samples[0].top_procs.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
