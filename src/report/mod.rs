pub mod charts;
pub mod html;

use std::collections::BTreeSet;

use anyhow::{ensure, Result};
use chrono::{DateTime, Local};

use crate::session::{Sample, Session};

/// CPU "throttling" is a heuristic (Windows exposes no direct signal):
/// a sample counts when overall usage is above `LOAD_PCT` while the fastest
/// core sits below `DROP_RATIO` of the loaded baseline clock.
const LOAD_PCT: f32 = 80.0;
const BASELINE_LOAD_PCT: f32 = 50.0;
const DROP_RATIO: f64 = 0.75;

pub struct Summary {
    pub started: DateTime<Local>,
    pub game: Option<String>,
    pub interval_secs: u64,
    pub duration_secs: u64,
    pub sample_count: usize,
    pub gpu_name: Option<String>,
    pub cpu_name: String,
    pub gpu: Option<GpuSummary>,
    pub cpu: CpuSummary,
    pub cpu_temp: Option<Series>,
    pub cpu_temp_source: Option<String>,
    pub ram_peak_mib: u64,
    pub ram_total_mib: Option<u64>,
}

pub struct Series {
    pub avg: f64,
    pub peak: f64,
    pub peak_at_secs: u64,
}

pub struct GpuSummary {
    pub temp_c: Series,
    pub core_mhz: Series,
    pub power_w: Series,
    pub util_pct: Series,
    pub vram_used_mib: Series,
    pub throttle: Vec<Throttle>,
}

pub struct Throttle {
    pub reason: String,
    pub seconds: u64,
    pub episodes: usize,
    pub longest_at_secs: u64,
}

pub struct CpuSummary {
    pub usage_pct: Series,
    pub max_core_mhz: Series,
    pub throttle: Option<CpuThrottle>,
}

pub struct CpuThrottle {
    pub baseline_mhz: u64,
    pub seconds: u64,
    pub episodes: usize,
    pub longest_at_secs: u64,
}

pub fn analyze(session: &Session) -> Result<Summary> {
    let samples = &session.samples;
    ensure!(!samples.is_empty(), "session contains no samples");

    let interval = session.meta.interval_secs.max(1);
    let t0 = samples[0].t_ms;
    let offsets: Vec<u64> = samples
        .iter()
        .map(|s| (s.t_ms - t0).max(0) as u64 / 1000)
        .collect();
    let duration_secs = offsets[offsets.len() - 1] + interval;

    Ok(Summary {
        started: session.meta.started,
        game: session.meta.game.clone(),
        interval_secs: interval,
        duration_secs,
        sample_count: samples.len(),
        gpu_name: session.meta.gpu_name.clone(),
        cpu_name: session.meta.cpu_name.clone(),
        gpu: analyze_gpu(samples, &offsets, interval),
        cpu: analyze_cpu(samples, &offsets, interval),
        cpu_temp: series(
            samples
                .iter()
                .zip(&offsets)
                .filter_map(|(s, &t)| s.cpu_temp_c.map(|c| (t, f64::from(c)))),
        ),
        cpu_temp_source: session.meta.cpu_temp_source.clone(),
        ram_peak_mib: samples.iter().map(|s| s.ram_used_mib).max().unwrap_or(0),
        ram_total_mib: session.meta.ram_total_mib,
    })
}

fn analyze_gpu(samples: &[Sample], offsets: &[u64], interval: u64) -> Option<GpuSummary> {
    let gpu: Vec<(u64, &crate::session::GpuSample)> = samples
        .iter()
        .zip(offsets)
        .filter_map(|(s, &t)| s.gpu.as_ref().map(|g| (t, g)))
        .collect();
    if gpu.is_empty() {
        return None;
    }

    let reasons: BTreeSet<&String> = gpu.iter().flat_map(|(_, g)| &g.throttle).collect();
    let throttle = reasons
        .into_iter()
        .filter_map(|reason| {
            let flags: Vec<bool> = gpu
                .iter()
                .map(|(_, g)| g.throttle.contains(reason))
                .collect();
            let runs = episodes(&flags);
            let longest = runs.iter().max_by_key(|(_, len)| *len)?;
            Some(Throttle {
                reason: reason.clone(),
                seconds: flags.iter().filter(|f| **f).count() as u64 * interval,
                episodes: runs.len(),
                longest_at_secs: gpu[longest.0].0,
            })
        })
        .collect();

    Some(GpuSummary {
        temp_c: series(gpu.iter().map(|(t, g)| (*t, f64::from(g.temp_c))))?,
        core_mhz: series(gpu.iter().map(|(t, g)| (*t, f64::from(g.core_mhz))))?,
        power_w: series(gpu.iter().map(|(t, g)| (*t, g.power_w)))?,
        util_pct: series(gpu.iter().map(|(t, g)| (*t, f64::from(g.util_pct))))?,
        vram_used_mib: series(gpu.iter().map(|(t, g)| (*t, g.vram_used_mib as f64)))?,
        throttle,
    })
}

fn max_core(s: &Sample) -> u64 {
    s.cpu.per_core_mhz.iter().copied().max().unwrap_or(0)
}

/// Per-sample CPU throttle flags, shared by the text summary and the HTML
/// chart shading so the heuristic lives in one place.
pub(crate) fn cpu_throttle_flags(samples: &[Sample]) -> Option<(u64, Vec<bool>)> {
    let mut loaded_clocks: Vec<u64> = samples
        .iter()
        .filter(|s| s.cpu.usage_pct > BASELINE_LOAD_PCT)
        .map(max_core)
        .collect();
    (!loaded_clocks.is_empty()).then(|| {
        loaded_clocks.sort_unstable();
        // 90th percentile rather than max, so one boost spike doesn't set an
        // unreachable baseline.
        let baseline = loaded_clocks[(loaded_clocks.len() - 1) * 9 / 10];
        let threshold = baseline as f64 * DROP_RATIO;
        let flags = samples
            .iter()
            .map(|s| s.cpu.usage_pct > LOAD_PCT && (max_core(s) as f64) < threshold)
            .collect();
        (baseline, flags)
    })
}

fn analyze_cpu(samples: &[Sample], offsets: &[u64], interval: u64) -> CpuSummary {
    let throttle = cpu_throttle_flags(samples).map(|(baseline, flags)| {
        let runs = episodes(&flags);
        CpuThrottle {
            baseline_mhz: baseline,
            seconds: flags.iter().filter(|f| **f).count() as u64 * interval,
            episodes: runs.len(),
            longest_at_secs: runs
                .iter()
                .max_by_key(|(_, len)| *len)
                .map(|&(start, _)| offsets[start])
                .unwrap_or(0),
        }
    });

    CpuSummary {
        usage_pct: series(
            samples
                .iter()
                .zip(offsets)
                .map(|(s, &t)| (t, f64::from(s.cpu.usage_pct))),
        )
        .unwrap_or(Series {
            avg: 0.0,
            peak: 0.0,
            peak_at_secs: 0,
        }),
        max_core_mhz: series(
            samples
                .iter()
                .zip(offsets)
                .map(|(s, &t)| (t, max_core(s) as f64)),
        )
        .unwrap_or(Series {
            avg: 0.0,
            peak: 0.0,
            peak_at_secs: 0,
        }),
        throttle,
    }
}

/// Which verdict colour the session earns. Thermal throttling is the red
/// case; power-limit throttling, estimated CPU drops, or merely running hot
/// are amber. A quiet session where the CPU check could not run (it needs
/// sustained load) is grey ("na"), not green — "nothing found" is not
/// "all clear".
fn verdict_level(
    thermal_episodes: usize,
    other_episodes: usize,
    cpu_estimated: usize,
    gpu_peak_c: Option<f64>,
    cpu_check_ran: bool,
) -> &'static str {
    if thermal_episodes > 0 {
        "bad"
    } else if other_episodes > 0 || cpu_estimated > 0 || gpu_peak_c.is_some_and(|p| p >= 85.0) {
        "warn"
    } else if cpu_check_ran {
        "good"
    } else {
        "na"
    }
}

/// Plain-language session verdict: a level ("good", "warn", "bad", "na")
/// and one or two sentences, shared by the HTML report and the GUI.
pub fn verdict(s: &Summary) -> (&'static str, String) {
    let (mut thermal, mut other) = (0usize, 0usize);
    let mut thermal_at = None;
    let mut thermal_most_secs = 0;
    if let Some(gpu) = &s.gpu {
        for t in &gpu.throttle {
            if t.reason.contains("thermal") || t.reason.contains("slowdown") {
                thermal += t.episodes;
                // "mostly around minute X" should come from the thermal
                // reason with the most throttle time, not the one that
                // happens to sort last.
                if t.seconds >= thermal_most_secs {
                    thermal_most_secs = t.seconds;
                    thermal_at = Some(t.longest_at_secs);
                }
            } else {
                other += t.episodes;
            }
        }
    }
    let cpu_est = s.cpu.throttle.as_ref().map_or(0, |t| t.episodes);
    let gpu_peak = s.gpu.as_ref().map(|g| g.temp_c.peak);
    let class = verdict_level(thermal, other, cpu_est, gpu_peak, s.cpu.throttle.is_some());

    let mut lines = Vec::new();
    if thermal > 0 {
        let when = thermal_at
            .map(|t| {
                if s.duration_secs >= 120 {
                    format!(", mostly around minute {}", t / 60)
                } else {
                    format!(", around {}", fmt_time(t))
                }
            })
            .unwrap_or_default();
        lines.push(format!(
            "Your GPU slowed itself down {thermal} time(s) to avoid overheating{when}."
        ));
    }
    if other > 0 {
        lines.push(format!(
            "Your GPU hit its power limit {other} time(s) — common on gaming laptops."
        ));
    }
    if cpu_est > 0 {
        lines.push(format!(
            "Your CPU appears to have slowed down {cpu_est} time(s) under heavy load (estimated)."
        ));
    }
    if lines.is_empty() {
        lines.push(if class == "na" {
            "Not enough load to check for slowdowns — record during a game for a real result."
                .to_string()
        } else {
            match gpu_peak {
                Some(p) if p >= 85.0 => {
                    format!("Your GPU ran hot (peak {p:.0} C) but did not slow itself down.")
                }
                Some(_) => "Your laptop stayed cool — no throttling detected.".to_string(),
                None => "No slowdowns detected. (No NVIDIA GPU was recorded in this session.)"
                    .to_string(),
            }
        });
    }
    (class, lines.join(" "))
}

fn series(points: impl Iterator<Item = (u64, f64)>) -> Option<Series> {
    let mut count = 0u64;
    let mut sum = 0.0;
    let mut peak = f64::NEG_INFINITY;
    let mut peak_at = 0;
    for (t, value) in points {
        count += 1;
        sum += value;
        if value > peak {
            peak = value;
            peak_at = t;
        }
    }
    (count > 0).then(|| Series {
        avg: sum / count as f64,
        peak,
        peak_at_secs: peak_at,
    })
}

/// Runs of consecutive `true` flags as (start index, length).
fn episodes(flags: &[bool]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, &flag) in flags.iter().enumerate() {
        match (flag, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push((s, i - s));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, flags.len() - s));
    }
    runs
}

pub fn print(s: &Summary) {
    println!(
        "Session {}{}",
        s.started.format("%Y-%m-%d %H:%M:%S"),
        s.game
            .as_deref()
            .map(|g| format!("  ({g})"))
            .unwrap_or_default()
    );
    println!(
        "Duration {}  ({} samples @ {}s)",
        fmt_time(s.duration_secs),
        s.sample_count,
        s.interval_secs
    );

    println!();
    match (&s.gpu, &s.gpu_name) {
        (Some(gpu), name) => {
            println!("GPU  {}", name.as_deref().unwrap_or("unknown"));
            print_series("Temperature", &gpu.temp_c, "C", 0);
            print_series("Core clock", &gpu.core_mhz, "MHz", 0);
            print_series("Utilisation", &gpu.util_pct, "%", 0);
            print_series("Power draw", &gpu.power_w, "W", 1);
            print_series("VRAM used", &gpu.vram_used_mib, "MiB", 0);
            if gpu.throttle.is_empty() {
                println!("  Throttling   none");
            } else {
                println!("  Throttling");
                for t in &gpu.throttle {
                    println!(
                        "    {}: {} episode(s), {} total, longest at {}",
                        t.reason,
                        t.episodes,
                        fmt_time(t.seconds),
                        fmt_time(t.longest_at_secs)
                    );
                }
            }
        }
        _ => println!("GPU  not recorded"),
    }

    println!();
    println!("CPU  {}", s.cpu_name);
    print_series("Usage", &s.cpu.usage_pct, "%", 0);
    print_series("Fastest core", &s.cpu.max_core_mhz, "MHz", 0);
    match &s.cpu_temp {
        Some(temp) => {
            print_series("Temperature", temp, "C", 0);
            if let Some(source) = &s.cpu_temp_source {
                println!("               (source: {source})");
            }
        }
        None => println!("  Temperature  not recorded"),
    }
    match &s.cpu.throttle {
        Some(t) if t.episodes > 0 => {
            println!(
                "  Clock drops under load (heuristic): {} episode(s), {} total, longest at {}",
                t.episodes,
                fmt_time(t.seconds),
                fmt_time(t.longest_at_secs)
            );
            println!(
                "    (fastest core below {:.0}% of the {}-MHz loaded baseline while usage > {:.0}%;",
                DROP_RATIO * 100.0,
                t.baseline_mhz,
                LOAD_PCT
            );
            println!(
                "     Windows exposes no direct CPU throttle signal, so treat as an estimate)"
            );
        }
        Some(_) => println!("  Clock drops under load (heuristic): none detected"),
        None => println!("  Clock drops under load (heuristic): not enough high-load samples"),
    }

    println!();
    println!("RAM  peak {} MiB", s.ram_peak_mib);
}

fn print_series(label: &str, series: &Series, unit: &str, decimals: usize) {
    println!(
        "  {label:<12} avg {avg:.decimals$} {unit}, peak {peak:.decimals$} {unit} at {at}",
        avg = series.avg,
        peak = series.peak,
        at = fmt_time(series.peak_at_secs),
    );
}

fn fmt_time(total_secs: u64) -> String {
    let (h, m, s) = (total_secs / 3600, total_secs % 3600 / 60, total_secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{CpuSample, GpuSample, SessionMeta};

    fn sample(i: i64, gpu_temp: u32, throttle: &[&str], usage: f32, max_mhz: u64) -> Sample {
        Sample {
            t_ms: i * 1000,
            cpu: CpuSample {
                usage_pct: usage,
                per_core_mhz: vec![max_mhz, max_mhz.saturating_sub(200)],
                per_core_usage: vec![usage, usage],
            },
            gpu: Some(GpuSample {
                temp_c: gpu_temp,
                core_mhz: 2500,
                mem_mhz: 8000,
                util_pct: 97,
                mem_util_pct: 60,
                power_w: 110.0,
                vram_used_mib: 6000,
                throttle: throttle.iter().map(|s| s.to_string()).collect(),
            }),
            cpu_temp_c: Some(70.0 + i as f32),
            ram_used_mib: 12_000 + i as u64,
        }
    }

    fn session(samples: Vec<Sample>) -> Session {
        Session {
            meta: SessionMeta {
                started: chrono::Local::now(),
                interval_secs: 1,
                cpu_name: "test cpu".into(),
                ram_total_mib: Some(16_003),
                gpu_name: Some("test gpu".into()),
                vram_total_mib: Some(8192),
                cpu_temp_source: Some("test".into()),
                game: None,
            },
            samples,
        }
    }

    #[test]
    fn empty_session_is_an_error() {
        assert!(analyze(&session(vec![])).is_err());
    }

    #[test]
    fn duration_covers_last_sample() {
        let s = session((0..60).map(|i| sample(i, 70, &[], 20.0, 4000)).collect());
        let summary = analyze(&s).unwrap();
        assert_eq!(summary.duration_secs, 60);
        assert_eq!(summary.sample_count, 60);
    }

    #[test]
    fn gpu_peak_temperature_and_time() {
        let mut samples: Vec<Sample> = (0..100).map(|i| sample(i, 75, &[], 20.0, 4000)).collect();
        samples[42] = sample(42, 87, &[], 20.0, 4000);
        let summary = analyze(&session(samples)).unwrap();
        let gpu = summary.gpu.unwrap();
        assert_eq!(gpu.temp_c.peak, 87.0);
        assert_eq!(gpu.temp_c.peak_at_secs, 42);
    }

    #[test]
    fn throttle_episodes_are_grouped_and_timed() {
        let thermal = "thermal limit (hardware)";
        let samples: Vec<Sample> = (0..40)
            .map(|i| {
                let reasons: &[&str] = if (10..15).contains(&i) || (30..32).contains(&i) {
                    &[thermal]
                } else {
                    &[]
                };
                sample(i, 80, reasons, 50.0, 4000)
            })
            .collect();
        let summary = analyze(&session(samples)).unwrap();
        let throttle = &summary.gpu.unwrap().throttle;
        assert_eq!(throttle.len(), 1);
        assert_eq!(throttle[0].reason, thermal);
        assert_eq!(throttle[0].episodes, 2);
        assert_eq!(throttle[0].seconds, 7);
        assert_eq!(throttle[0].longest_at_secs, 10);
    }

    #[test]
    fn cpu_clock_drop_under_load_is_flagged() {
        let samples: Vec<Sample> = (0..100)
            .map(|i| {
                let mhz = if (50..56).contains(&i) { 2500 } else { 4000 };
                sample(i, 70, &[], 90.0, mhz)
            })
            .collect();
        let summary = analyze(&session(samples)).unwrap();
        let throttle = summary.cpu.throttle.unwrap();
        assert_eq!(throttle.baseline_mhz, 4000);
        assert_eq!(throttle.episodes, 1);
        assert_eq!(throttle.seconds, 6);
        assert_eq!(throttle.longest_at_secs, 50);
    }

    #[test]
    fn idle_clock_drops_are_not_throttling() {
        // Clocks drop while usage is low: power saving, not throttling.
        let samples: Vec<Sample> = (0..100)
            .map(|i| {
                let (usage, mhz) = if i < 50 { (90.0, 4000) } else { (5.0, 1200) };
                sample(i, 70, &[], usage, mhz)
            })
            .collect();
        let summary = analyze(&session(samples)).unwrap();
        assert_eq!(summary.cpu.throttle.unwrap().episodes, 0);
    }

    #[test]
    fn verdict_levels() {
        assert_eq!(verdict_level(1, 0, 0, Some(70.0), true), "bad");
        assert_eq!(verdict_level(0, 2, 0, Some(70.0), true), "warn");
        assert_eq!(verdict_level(0, 0, 1, None, true), "warn");
        assert_eq!(verdict_level(0, 0, 0, Some(86.0), false), "warn");
        assert_eq!(verdict_level(0, 0, 0, Some(70.0), true), "good");
        assert_eq!(verdict_level(0, 0, 0, None, true), "good");
    }

    #[test]
    fn verdict_moment_follows_dominant_thermal_reason() {
        let series = |v: f64| Series {
            avg: v,
            peak: v,
            peak_at_secs: 0,
        };
        let summary = Summary {
            started: chrono::Local::now(),
            game: None,
            interval_secs: 1,
            duration_secs: 3600,
            sample_count: 3600,
            gpu_name: Some("g".into()),
            cpu_name: "c".into(),
            gpu: Some(GpuSummary {
                temp_c: series(80.0),
                core_mhz: series(2000.0),
                power_w: series(100.0),
                util_pct: series(95.0),
                vram_used_mib: series(5000.0),
                throttle: vec![
                    // Dominant: 10 minutes of throttle around minute 5.
                    Throttle {
                        reason: "thermal limit (hardware)".into(),
                        seconds: 600,
                        episodes: 3,
                        longest_at_secs: 300,
                    },
                    // Sorts last alphabetically but is a sideshow.
                    Throttle {
                        reason: "thermal limit (software)".into(),
                        seconds: 60,
                        episodes: 1,
                        longest_at_secs: 2400,
                    },
                ],
            }),
            cpu: CpuSummary {
                usage_pct: series(50.0),
                max_core_mhz: series(4000.0),
                throttle: None,
            },
            cpu_temp: None,
            cpu_temp_source: None,
            ram_peak_mib: 1000,
            ram_total_mib: None,
        };
        let (class, text) = verdict(&summary);
        assert_eq!(class, "bad");
        assert!(text.contains("minute 5"), "{text}");
    }

    #[test]
    fn quiet_session_is_not_declared_clean() {
        // An idle session gives the CPU check nothing to evaluate; that is
        // "could not check", not "all clear".
        assert_eq!(verdict_level(0, 0, 0, None, false), "na");
        assert_eq!(verdict_level(0, 0, 0, Some(55.0), false), "na");
    }

    #[test]
    fn episodes_handles_runs_at_the_end() {
        assert_eq!(
            episodes(&[false, true, true, false, true]),
            vec![(1, 2), (4, 1)]
        );
        assert!(episodes(&[false, false]).is_empty());
    }
}
