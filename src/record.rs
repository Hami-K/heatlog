use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use heatlog::config::{self, Config};
use heatlog::recorder::{self, Recorder};

pub fn run(
    cfg: &Config,
    auto: bool,
    interval_override: Option<u64>,
    out_override: Option<PathBuf>,
    no_tui: bool,
) -> Result<()> {
    use std::io::IsTerminal as _;
    let interval_secs = interval_override.unwrap_or(cfg.interval_secs).max(1);
    let sessions_dir = match out_override {
        Some(dir) => dir,
        None => cfg.sessions_dir()?,
    };

    let stop = Arc::new(AtomicBool::new(false));
    {
        // cloned so the handler keeps its own reference across threads
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .context("installing Ctrl-C handler")?;
    }

    let game = if auto {
        if cfg.games.is_empty() {
            let path = config::config_path()?;
            bail!(
                "auto mode needs process names in the `games` list of {}.\n\
                 Find the exact name in Task Manager (Details tab, Name column) while the game runs.",
                path.display()
            );
        }
        println!(
            "Waiting for one of {:?} to start (Ctrl-C to cancel)",
            cfg.games
        );
        match recorder::wait_for_game(&cfg.games, &stop) {
            Some((pid, name)) => {
                println!("{name} detected (pid {pid})");
                Some((pid, name))
            }
            None => return Ok(()), // cancelled while waiting
        }
    } else {
        None
    };

    let mut rec = Recorder::start(&cfg.lhm_url, &sessions_dir, interval_secs, game)?;
    if !rec.gpu_available() {
        println!("GPU unavailable, recording CPU/RAM only");
    }
    match rec.cpu_temp_source() {
        Some(label) => println!("CPU temperature source: {label}"),
        None => println!(
            "CPU temperature: unavailable (start LibreHardwareMonitor's web server to enable it)"
        ),
    }
    println!("Recording to {} (Ctrl-C to stop)", rec.path().display());

    let mut tui = (!no_tui && std::io::stdout().is_terminal()).then(crate::tui::Tui::start);
    let interval = Duration::from_secs(interval_secs);
    let started = std::time::Instant::now();
    let mut tick: u32 = 1;
    let mut exit_note = None;
    while !stop.load(Ordering::SeqCst) {
        // Sleep until an absolute tick so sensor-read time doesn't stretch
        // the interval; skip ticks that are already in the past.
        let mut target = started + interval * tick;
        let now = std::time::Instant::now();
        while target <= now {
            tick += 1;
            target = started + interval * tick;
        }
        match &mut tui {
            Some(tui) => {
                if tui.wait(target)? {
                    break;
                }
            }
            None => std::thread::sleep(target - now),
        }
        tick += 1;
        if stop.load(Ordering::SeqCst) {
            break;
        }

        let step = rec.step()?;
        match &mut tui {
            Some(tui) => tui.draw(
                &step.sample,
                rec.count(),
                started.elapsed(),
                &rec.path().display().to_string(),
            )?,
            None => print_status(
                rec.count(),
                step.sample.cpu.usage_pct,
                step.sample.gpu.as_ref().map(|g| g.temp_c),
                step.sample.cpu_temp_c,
            ),
        }

        if let Some(name) = step.game_exited {
            exit_note = Some(format!("{name} exited, stopping"));
            break;
        }
    }

    drop(tui); // leave the alternate screen before printing the summary
    println!();
    if let Some(note) = exit_note {
        println!("{note}");
    }
    println!("Saved {} samples to {}", rec.count(), rec.path().display());
    Ok(())
}

fn print_status(n: u64, cpu_pct: f32, gpu_temp: Option<u32>, cpu_temp: Option<f32>) {
    let gpu = match gpu_temp {
        Some(t) => format!("GPU {t} °C"),
        None => "GPU n/a".to_string(),
    };
    let cpu_t = match cpu_temp {
        Some(t) => format!("CPU {t:.0} °C"),
        None => "CPU temp n/a".to_string(),
    };
    print!("\rsample {n}  CPU {cpu_pct:.0}%  {cpu_t}  {gpu}   ");
    let _ = std::io::stdout().flush();
}
