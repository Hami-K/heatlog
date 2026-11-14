use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Local, Utc};
use sysinfo::{Pid, ProcessesToUpdate, System};

use heatlog::config::{self, Config};
use heatlog::sensors::cpu::CpuSensor;
use heatlog::sensors::cpu_temp::{self, CpuTempSource};
use heatlog::sensors::gpu::GpuSensor;
use heatlog::session::{Sample, SessionMeta, SessionWriter};

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
        match wait_for_game(cfg, &stop)? {
            Some(found) => Some(found),
            None => return Ok(()), // cancelled while waiting
        }
    } else {
        None
    };

    let gpu = match GpuSensor::new() {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            println!("GPU unavailable, recording CPU/RAM only");
            None
        }
    };
    let mut cpu = CpuSensor::new();
    let mut temp_source: Option<Box<dyn CpuTempSource>> = cpu_temp::detect(&cfg.lhm_url);
    match &temp_source {
        Some(src) => println!("CPU temperature source: {}", src.label()),
        None => println!(
            "CPU temperature: unavailable (start LibreHardwareMonitor's web server to enable it)"
        ),
    }

    let gpu_info = gpu.as_ref().and_then(|g| g.info().ok());
    let first_read = cpu.read();
    let meta = SessionMeta {
        started: Local::now(),
        interval_secs,
        cpu_name: first_read.name,
        ram_total_mib: Some(first_read.ram_total_mib),
        gpu_name: gpu_info.as_ref().map(|g| g.name.clone()),
        vram_total_mib: gpu_info.as_ref().map(|g| g.vram_total_mib),
        cpu_temp_source: temp_source.as_ref().map(|s| s.label().to_string()),
        game: game.as_ref().map(|(_, name)| name.clone()),
    };
    let mut writer = SessionWriter::create(&sessions_dir, &meta)?;
    println!("Recording to {} (Ctrl-C to stop)", writer.path().display());

    let mut tui = (!no_tui && std::io::stdout().is_terminal()).then(crate::tui::Tui::start);
    let interval = Duration::from_secs(interval_secs);
    let started = std::time::Instant::now();
    let mut process_watch = System::new();
    let mut count: u64 = 0;
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

        let cpu_snap = cpu.read();
        let gpu_snap = gpu.as_ref().and_then(|g| g.read().ok());
        let temp = temp_source.as_mut().and_then(|s| s.read_c().ok());
        let sample = Sample::new(Utc::now(), cpu_snap, gpu_snap, temp);
        writer.append(&sample)?;
        count += 1;
        match &mut tui {
            Some(tui) => tui.draw(
                &sample,
                count,
                started.elapsed(),
                &writer.path().display().to_string(),
            )?,
            None => print_status(
                count,
                sample.cpu.usage_pct,
                sample.gpu.as_ref().map(|g| g.temp_c),
                temp,
            ),
        }

        if let Some((pid, name)) = &game {
            process_watch.refresh_processes(ProcessesToUpdate::All, true);
            if process_watch.process(*pid).is_none() {
                exit_note = Some(format!("{name} exited, stopping"));
                break;
            }
        }
    }

    drop(tui); // leave the alternate screen before printing the summary
    println!();
    if let Some(note) = exit_note {
        println!("{note}");
    }
    println!("Saved {count} samples to {}", writer.path().display());
    Ok(())
}

fn print_status(n: u64, cpu_pct: f32, gpu_temp: Option<u32>, cpu_temp: Option<f32>) {
    let gpu = match gpu_temp {
        Some(t) => format!("GPU {t} C"),
        None => "GPU n/a".to_string(),
    };
    let cpu_t = match cpu_temp {
        Some(t) => format!("CPU {t:.0} C"),
        None => "CPU temp n/a".to_string(),
    };
    print!("\rsample {n}  CPU {cpu_pct:.0}%  {cpu_t}  {gpu}   ");
    let _ = std::io::stdout().flush();
}

fn wait_for_game(cfg: &Config, stop: &AtomicBool) -> Result<Option<(Pid, String)>> {
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
    let mut sys = System::new();
    while !stop.load(Ordering::SeqCst) {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        for process in sys.processes().values() {
            let name = process.name().to_string_lossy();
            if cfg.games.iter().any(|g| g.eq_ignore_ascii_case(&name)) {
                println!("{name} detected (pid {})", process.pid());
                return Ok(Some((process.pid(), name.into_owned())));
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Ok(None)
}
