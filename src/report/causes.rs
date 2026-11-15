//! What might explain the slow moments in a session. These are
//! suggestions read off the recording, never certainties: the tool can
//! see that a program was busy while the GPU throttled, not that it was
//! to blame.

use std::collections::HashMap;

use super::{cpu_throttle_flags, Cooling, Summary};
use crate::session::Session;

pub struct Cause {
    /// A few words naming the suspect.
    pub headline: String,
    /// One sentence a non-technical reader can act on.
    pub detail: String,
}

/// Samples where something was holding performance back: the GPU said so,
/// or the CPU clock-drop estimate fired.
fn slow_samples(session: &Session) -> Vec<bool> {
    let cpu_flags = cpu_throttle_flags(&session.samples)
        .map(|(_, flags)| flags)
        .unwrap_or_default();
    session
        .samples
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let gpu = sample
                .gpu
                .as_ref()
                .is_some_and(|gpu| !gpu.throttle.is_empty());
            gpu || cpu_flags.get(i).copied().unwrap_or(false)
        })
        .collect()
}

/// The busiest programs during the slow moments, loudest first, excluding
/// the game itself: it is supposed to be busy.
fn busiest_during(session: &Session, slow: &[bool], game: Option<&str>) -> Vec<(String, f32)> {
    let mut totals: HashMap<&str, (f32, u32)> = HashMap::new();
    for (sample, _) in session.samples.iter().zip(slow).filter(|(_, s)| **s) {
        for process in &sample.top_procs {
            let entry = totals.entry(process.name.as_str()).or_insert((0.0, 0));
            entry.0 += process.cpu_pct;
            entry.1 += 1;
        }
    }
    let mut busiest: Vec<(String, f32)> = totals
        .into_iter()
        .filter(|(name, _)| !game.is_some_and(|game| game.eq_ignore_ascii_case(name)))
        .map(|(name, (total, count))| (name.to_string(), total / count.max(1) as f32))
        .collect();
    busiest.sort_by(|a, b| b.1.total_cmp(&a.1));
    busiest
}

/// Reads the session for things that plausibly explain its slow moments.
/// An empty list means nothing stood out, not that nothing happened.
pub fn causes(session: &Session, summary: &Summary) -> Vec<Cause> {
    let slow = slow_samples(session);
    if !slow.iter().any(|&s| s) {
        return Vec::new();
    }
    let mut causes = Vec::new();

    // Heat and power limits come straight from the driver.
    if let Some(gpu) = &summary.gpu {
        let thermal: u64 = gpu
            .throttle
            .iter()
            .filter(|t| t.reason.contains("thermal") || t.reason.contains("slowdown"))
            .map(|t| t.seconds)
            .sum();
        if thermal > 0 {
            causes.push(Cause {
                headline: "Heat".to_string(),
                detail: format!(
                    "The graphics chip reached its temperature limit and slowed down to \
                     protect itself, for {} in total. Peak {:.0} °C.",
                    super::fmt_time(thermal),
                    gpu.temp_c.peak
                ),
            });
        }
        let power: u64 = gpu
            .throttle
            .iter()
            .filter(|t| t.reason.contains("power"))
            .map(|t| t.seconds)
            .sum();
        if power > 0 {
            causes.push(Cause {
                headline: "Power limit".to_string(),
                detail: format!(
                    "The graphics chip was held back by its power limit for {}. This is \
                     normal on a laptop, and a higher performance mode may raise it.",
                    super::fmt_time(power)
                ),
            });
        }
    }

    if summary.cooling == Cooling::AtLimit {
        causes.push(Cause {
            headline: "Cooling limit".to_string(),
            detail: "The fans were already at full speed, so the machine was cooling as hard \
                     as it can. Clearing dust or raising the laptop can help a little; \
                     nothing else will."
                .to_string(),
        });
    } else if summary.cooling == Cooling::FansHeldBack {
        causes.push(Cause {
            headline: "Fans held back".to_string(),
            detail: "Things slowed down while the fans still had room to spin faster. A fan \
                     curve or power plan may be limiting them - the laptop maker's utility \
                     is where to look."
                .to_string(),
        });
    }

    // A background program that was busy while things were slow.
    for (name, cpu_pct) in busiest_during(session, &slow, summary.game.as_deref())
        .into_iter()
        .filter(|(_, cpu_pct)| *cpu_pct >= 10.0)
        .take(2)
    {
        causes.push(Cause {
            headline: format!("Background app: {name}"),
            detail: format!(
                "{name} was using about {cpu_pct:.0}% of the processor during the slow \
                 moments. Closing it before playing may help."
            ),
        });
    }

    if let Some(total) = summary.ram_total_mib.filter(|total| *total > 0) {
        let share = summary.ram_peak_mib as f64 / total as f64;
        if share >= 0.9 {
            causes.push(Cause {
                headline: "Memory nearly full".to_string(),
                detail: format!(
                    "Memory use peaked at {:.0}% of what the machine has. When it runs out, \
                     Windows starts using the disk instead, which stutters.",
                    share * 100.0
                ),
            });
        }
    }
    causes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report;
    use crate::session::{CpuSample, GpuSample, ProcSample, Sample, SessionMeta};

    fn sample(i: i64, throttle: &[&str], procs: &[(&str, f32)], ram_mib: u64) -> Sample {
        Sample {
            t_ms: i * 1000,
            cpu: CpuSample {
                usage_pct: 50.0,
                per_core_mhz: vec![4000],
                per_core_usage: vec![50.0],
            },
            gpu: Some(GpuSample {
                temp_c: 86,
                core_mhz: 2400,
                mem_mhz: 8000,
                util_pct: 98,
                mem_util_pct: 60,
                power_w: 110.0,
                vram_used_mib: 6000,
                throttle: throttle.iter().map(|t| t.to_string()).collect(),
            }),
            cpu_temp_c: Some(75.0),
            ram_used_mib: ram_mib,
            fans: vec![crate::session::Fan {
                name: "CPU Fan".into(),
                rpm: 5000,
            }],
            top_procs: procs
                .iter()
                .map(|(name, cpu_pct)| ProcSample {
                    name: (*name).to_string(),
                    cpu_pct: *cpu_pct,
                    ram_mib: 500,
                })
                .collect(),
        }
    }

    fn session(samples: Vec<Sample>) -> Session {
        Session {
            meta: SessionMeta {
                started: chrono::Local::now(),
                interval_secs: 1,
                cpu_name: "test".into(),
                ram_total_mib: Some(16_000),
                gpu_name: Some("test gpu".into()),
                vram_total_mib: Some(8192),
                cpu_temp_source: Some("test".into()),
                game: Some("Game.exe".into()),
            },
            samples,
        }
    }

    #[test]
    fn a_busy_background_app_during_a_slowdown_is_suggested() {
        let samples = (0..40)
            .map(|i| {
                let throttling: &[&str] = if (10..20).contains(&i) {
                    &["thermal limit (hardware)"]
                } else {
                    &[]
                };
                // The game is always busy; the browser only during the
                // throttled stretch.
                let procs: &[(&str, f32)] = if (10..20).contains(&i) {
                    &[("Game.exe", 60.0), ("chrome.exe", 32.0)]
                } else {
                    &[("Game.exe", 60.0)]
                };
                sample(i, throttling, procs, 8_000)
            })
            .collect();
        let session = session(samples);
        let summary = report::analyze(&session).unwrap();
        let found = causes(&session, &summary);
        let headlines: Vec<&str> = found.iter().map(|c| c.headline.as_str()).collect();

        assert!(headlines.contains(&"Heat"), "{headlines:?}");
        assert!(
            headlines.iter().any(|h| h.contains("chrome.exe")),
            "{headlines:?}"
        );
        // The game itself is never blamed for being busy.
        assert!(
            !headlines.iter().any(|h| h.contains("Game.exe")),
            "{headlines:?}"
        );
    }

    #[test]
    fn a_quiet_session_suggests_nothing() {
        let samples = (0..30)
            .map(|i| sample(i, &[], &[("Game.exe", 40.0)], 8_000))
            .collect();
        let session = session(samples);
        let summary = report::analyze(&session).unwrap();
        assert!(causes(&session, &summary).is_empty());
    }

    #[test]
    fn full_memory_is_called_out() {
        let samples = (0..30)
            .map(|i| {
                let throttling: &[&str] = if i > 5 {
                    &["power limit (software)"]
                } else {
                    &[]
                };
                sample(i, throttling, &[("Game.exe", 40.0)], 15_000)
            })
            .collect();
        let session = session(samples);
        let summary = report::analyze(&session).unwrap();
        let headlines: Vec<String> = causes(&session, &summary)
            .into_iter()
            .map(|c| c.headline)
            .collect();
        assert!(
            headlines.iter().any(|h| h == "Power limit"),
            "{headlines:?}"
        );
        assert!(
            headlines.iter().any(|h| h == "Memory nearly full"),
            "{headlines:?}"
        );
    }
}
