use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

pub use crate::sensors::cpu::CpuSample;
use crate::sensors::cpu::CpuSnapshot;
pub use crate::sensors::gpu::GpuSample;

/// First line of every session file; per-sample lines follow.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub started: DateTime<Local>,
    pub interval_secs: u64,
    pub cpu_name: String,
    // Option because older session files predate the field (serde reads a
    // missing Option as None).
    pub ram_total_mib: Option<u64>,
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

impl Sample {
    pub fn new(
        at: DateTime<Utc>,
        cpu: CpuSnapshot,
        gpu: Option<GpuSample>,
        cpu_temp_c: Option<f32>,
    ) -> Self {
        Self {
            t_ms: at.timestamp_millis(),
            ram_used_mib: cpu.ram_used_mib,
            cpu: cpu.sample,
            gpu,
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
