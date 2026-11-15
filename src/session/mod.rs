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

pub struct SessionPeek {
    pub meta: SessionMeta,
    pub sample_count: usize,
    pub duration_secs: u64,
}

/// Listing-grade summary of a session, parsing only the metadata and the
/// first and last sample lines — `read` would JSON-parse every sample,
/// which is too slow across months of accumulated sessions.
pub fn peek(path: &Path) -> Result<SessionPeek> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let meta: SessionMeta = serde_json::from_str(
        lines
            .next()
            .with_context(|| format!("{} is empty", path.display()))?,
    )
    .context("parsing session metadata (line 1)")?;

    let rest: Vec<&str> = lines.collect();
    let first_t = rest
        .first()
        .and_then(|line| serde_json::from_str::<Sample>(line).ok())
        .map(|s| s.t_ms);
    // Search from the end past a possibly truncated final line.
    let last = rest.iter().enumerate().rev().find_map(|(i, line)| {
        serde_json::from_str::<Sample>(line)
            .ok()
            .map(|s| (i, s.t_ms))
    });

    let (sample_count, duration_secs) = match (first_t, last) {
        (Some(first), Some((last_idx, last_t))) => (
            last_idx + 1,
            (last_t - first).max(0) as u64 / 1000 + meta.interval_secs,
        ),
        _ => (0, 0),
    };
    Ok(SessionPeek {
        meta,
        sample_count,
        duration_secs,
    })
}

/// Session files in `dir`, oldest first. Ordered by modified time, not
/// name: "newest session" must stay correct even when a foreign-named
/// file lands in the folder.
pub fn list_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if dir.exists() {
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((modified, path));
            }
        }
    }
    files.sort();
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

/// Bytes used by session files and their exported reports.
pub fn total_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok()?.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum()
}

/// Removes a session and any HTML report exported beside it. Goes to the
/// Recycle Bin when Windows allows it, deleting outright only if that
/// fails (callers warn about that case).
pub fn delete(path: &Path) -> Result<()> {
    let report = path.with_extension("html");
    for file in [path, &report] {
        if !file.exists() {
            continue;
        }
        if trash::delete(file).is_err() {
            fs::remove_file(file).with_context(|| format!("deleting {}", file.display()))?;
        }
    }
    Ok(())
}

/// Deletes sessions last written more than `days` days ago; 0 means never.
/// Returns how many were removed.
pub fn delete_older_than(dir: &Path, days: u64) -> Result<usize> {
    if days == 0 {
        return Ok(0);
    }
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(days * 24 * 60 * 60))
        .context("age limit is too large")?;
    let mut removed = 0;
    for path in list_files(dir)? {
        let stale = path
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified < cutoff);
        if stale {
            delete(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub struct SessionWriter {
    out: BufWriter<File>,
    path: PathBuf,
}

impl SessionWriter {
    pub fn create(dir: &Path, meta: &SessionMeta) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let stamp = meta.started.format("%Y-%m-%d_%H-%M-%S");
        // Filenames have second resolution; create_new plus a suffix keeps
        // two recorders started in the same second from clobbering one file.
        let (file, path) = (0..10)
            .find_map(|n| {
                let name = if n == 0 {
                    format!("{stamp}.jsonl")
                } else {
                    format!("{stamp}-{n}.jsonl")
                };
                let path = dir.join(name);
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(file) => Some(Ok((file, path))),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(e) => Some(Err(e)),
                }
            })
            .with_context(|| format!("no free session filename for {stamp} in {}", dir.display()))?
            .with_context(|| format!("creating session file in {}", dir.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("heatlog-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            started: Local::now(),
            interval_secs: 1,
            cpu_name: "test".into(),
            ram_total_mib: None,
            gpu_name: None,
            vram_total_mib: None,
            cpu_temp_source: None,
            game: None,
        }
    }

    fn sample_line(t_ms: i64) -> String {
        serde_json::to_string(&Sample {
            t_ms,
            cpu: CpuSample {
                usage_pct: 10.0,
                per_core_mhz: vec![1000],
                per_core_usage: vec![10.0],
            },
            gpu: None,
            cpu_temp_c: None,
            ram_used_mib: 1,
        })
        .unwrap()
    }

    #[test]
    fn read_tolerates_a_truncated_final_line_only() {
        let dir = temp_dir("read");
        let path = dir.join("s.jsonl");
        let meta_line = serde_json::to_string(&meta()).unwrap();

        // A recorder killed mid-write leaves a torn last line: accepted.
        fs::write(
            &path,
            format!(
                "{meta_line}\n{}\n{}\n{{\"t_ms\":99",
                sample_line(0),
                sample_line(1000)
            ),
        )
        .unwrap();
        let session = read(&path).unwrap();
        assert_eq!(session.samples.len(), 2);

        // Corruption anywhere else means a broken file: rejected.
        fs::write(
            &path,
            format!("{meta_line}\ngarbage\n{}", sample_line(1000)),
        )
        .unwrap();
        assert!(read(&path).is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn peek_matches_read_without_parsing_everything() {
        let dir = temp_dir("peek");
        let path = dir.join("s.jsonl");
        let meta_line = serde_json::to_string(&meta()).unwrap();
        fs::write(
            &path,
            format!(
                "{meta_line}\n{}\n{}\n{}\n{{\"torn",
                sample_line(0),
                sample_line(1000),
                sample_line(9500)
            ),
        )
        .unwrap();

        let peeked = peek(&path).unwrap();
        assert_eq!(peeked.sample_count, 3);
        assert_eq!(peeked.duration_secs, 10); // 9.5s span + 1s interval
        assert_eq!(peeked.meta.interval_secs, 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn newest_session_is_by_modified_time_not_name() {
        let dir = temp_dir("order");
        // "zzz" sorts after any date-named file; it must not win on name.
        fs::write(dir.join("zzz.jsonl"), "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(dir.join("2026-01-01_00-00-00.jsonl"), "new").unwrap();

        let files = list_files(&dir).unwrap();
        let newest = files.last().unwrap().file_name().unwrap().to_string_lossy();
        assert_eq!(newest, "2026-01-01_00-00-00.jsonl");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_a_session_takes_its_exported_report_with_it() {
        let dir = temp_dir("delete");
        let session = dir.join("2026-01-01_00-00-00.jsonl");
        let report = dir.join("2026-01-01_00-00-00.html");
        fs::write(&session, "{}").unwrap();
        fs::write(&report, "<html>").unwrap();

        delete(&session).unwrap();
        assert!(!session.exists(), "session file still there");
        assert!(!report.exists(), "exported report left behind");
        // Deleting a session that is already gone is not an error.
        delete(&session).unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_second_sessions_get_distinct_files() {
        let dir = temp_dir("collide");
        let meta = meta();
        let first = SessionWriter::create(&dir, &meta).unwrap();
        let second = SessionWriter::create(&dir, &meta).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().exists() && second.path().exists());
        fs::remove_dir_all(&dir).ok();
    }
}
