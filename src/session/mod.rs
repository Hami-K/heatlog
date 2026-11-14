use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::sensors::cpu::CpuSnapshot;
use crate::sensors::gpu::GpuSnapshot;

/// First line of every session file; per-sample lines follow.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub started: DateTime<Local>,
    pub interval_secs: u64,
    pub cpu_name: String,
    pub gpu_name: Option<String>,
    pub vram_total_mib: Option<u64>,
    pub cpu_temp_source: Option<String>,
    pub game: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Sample {
    /// Milliseconds since the Unix epoch, UTC.
    pub t_ms: i64,
    pub cpu: CpuSample,
    pub gpu: Option<GpuSample>,
    pub cpu_temp_c: Option<f32>,
    pub ram_used_mib: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CpuSample {
    pub usage_pct: f32,
    pub per_core_mhz: Vec<u64>,
    pub per_core_usage: Vec<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GpuSample {
    pub temp_c: u32,
    pub core_mhz: u32,
    pub mem_mhz: u32,
    pub util_pct: u32,
    pub mem_util_pct: u32,
    pub power_w: f64,
    pub vram_used_mib: u64,
    pub throttle: Vec<String>,
}

impl Sample {
    pub fn new(
        at: DateTime<Utc>,
        cpu: CpuSnapshot,
        gpu: Option<GpuSnapshot>,
        cpu_temp_c: Option<f32>,
    ) -> Self {
        Self {
            t_ms: at.timestamp_millis(),
            ram_used_mib: cpu.ram_used_mib,
            cpu: CpuSample {
                usage_pct: cpu.usage_pct,
                per_core_mhz: cpu.per_core_mhz,
                per_core_usage: cpu.per_core_usage,
            },
            gpu: gpu.map(|g| GpuSample {
                temp_c: g.temp_c,
                core_mhz: g.core_clock_mhz,
                mem_mhz: g.mem_clock_mhz,
                util_pct: g.gpu_util_pct,
                mem_util_pct: g.mem_util_pct,
                power_w: g.power_w,
                vram_used_mib: g.vram_used_mib,
                throttle: g.throttle_reasons,
            }),
            cpu_temp_c,
        }
    }
}

pub struct Session {
    pub meta: SessionMeta,
    pub samples: Vec<Sample>,
}

pub fn read(path: &Path) -> Result<Session> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .collect::<Vec<_>>();
    anyhow::ensure!(!lines.is_empty(), "{} is empty", path.display());

    let (_, meta_line) = lines.remove(0);
    let meta: SessionMeta =
        serde_json::from_str(meta_line).context("parsing session metadata (line 1)")?;

    let mut samples = Vec::with_capacity(lines.len());
    let last = lines.len();
    for (pos, (line_no, line)) in lines.iter().enumerate() {
        match serde_json::from_str::<Sample>(line) {
            Ok(sample) => samples.push(sample),
            // A truncated final line is normal if the recorder was killed
            // mid-write; anywhere else it means a corrupt file.
            Err(_) if pos + 1 == last => {}
            Err(e) => {
                return Err(e).with_context(|| format!("parsing sample on line {}", line_no + 1))
            }
        }
    }
    Ok(Session { meta, samples })
}

/// Session files in `dir`, sorted oldest first (timestamped names sort
/// chronologically).
pub fn list_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if dir.exists() {
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

pub struct SessionWriter {
    out: BufWriter<File>,
    path: PathBuf,
}

impl SessionWriter {
    pub fn create(dir: &Path, meta: &SessionMeta) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!(
            "{}.jsonl",
            meta.started.format("%Y-%m-%d_%H-%M-%S")
        ));
        let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        let mut out = BufWriter::new(file);
        writeln!(out, "{}", serde_json::to_string(meta)?)?;
        out.flush()?;
        Ok(Self { out, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flushes after every line so an abrupt exit loses at most the sample
    /// being written.
    pub fn append(&mut self, sample: &Sample) -> Result<()> {
        writeln!(self.out, "{}", serde_json::to_string(sample)?)?;
        self.out.flush()?;
        Ok(())
    }
}
