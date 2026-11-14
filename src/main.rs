mod record;

use anyhow::Result;
use clap::{Parser, Subcommand};
use heatlog::{config, report, sensors, session};

#[derive(Parser)]
#[command(
    name = "heatlog",
    version,
    about = "Records hardware stats while you game and reports on them afterwards"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read all sensors once and print the current values
    Snapshot,
    /// Record a session to a timestamped file until stopped
    Record {
        /// Wait for a configured game process and record while it runs
        #[arg(long)]
        auto: bool,
        /// Seconds between samples (overrides the config file)
        #[arg(long)]
        interval: Option<u64>,
        /// Directory to write the session file to (overrides the config file)
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Summarise a recorded session (defaults to the most recent)
    Report {
        /// Session file: a path, or a name from `heatlog list`
        session: Option<String>,
    },
    /// Show recorded sessions
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Snapshot => snapshot(),
        Command::Record {
            auto,
            interval,
            out,
        } => {
            let cfg = config::load_or_create()?;
            record::run(&cfg, auto, interval, out)
        }
        Command::Report { session } => {
            let cfg = config::load_or_create()?;
            let path = resolve_session(&cfg, session.as_deref())?;
            let session = session::read(&path)?;
            report::print(&report::analyze(&session)?);
            Ok(())
        }
        Command::List => {
            let cfg = config::load_or_create()?;
            list_sessions(&cfg)
        }
    }
}

fn resolve_session(cfg: &config::Config, arg: Option<&str>) -> Result<std::path::PathBuf> {
    use anyhow::Context as _;
    let dir = cfg.sessions_dir()?;
    match arg {
        Some(arg) => {
            let direct = std::path::PathBuf::from(arg);
            if direct.exists() {
                return Ok(direct);
            }
            let name = if arg.ends_with(".jsonl") {
                arg.to_string()
            } else {
                format!("{arg}.jsonl")
            };
            let candidate = dir.join(name);
            anyhow::ensure!(
                candidate.exists(),
                "no session named {arg} in {}",
                dir.display()
            );
            Ok(candidate)
        }
        None => session::list_files(&dir)?
            .pop()
            .with_context(|| format!("no sessions recorded yet (looked in {})", dir.display())),
    }
}

fn list_sessions(cfg: &config::Config) -> Result<()> {
    let dir = cfg.sessions_dir()?;
    let files = session::list_files(&dir)?;
    if files.is_empty() {
        println!("No sessions recorded yet (looked in {})", dir.display());
        return Ok(());
    }
    println!("Sessions in {}", dir.display());
    for path in files {
        match session::read(&path) {
            Ok(s) => {
                let duration = s
                    .samples
                    .last()
                    .map(|last| {
                        (last.t_ms - s.samples[0].t_ms).max(0) as u64 / 1000 + s.meta.interval_secs
                    })
                    .unwrap_or(0);
                let mins = duration / 60;
                let secs = duration % 60;
                println!(
                    "  {}  {mins}:{secs:02}  {} samples  {}",
                    s.meta.started.format("%Y-%m-%d %H:%M:%S"),
                    s.samples.len(),
                    s.meta.game.as_deref().unwrap_or("-"),
                );
            }
            Err(e) => println!("  {}  (unreadable: {e:#})", path.display()),
        }
    }
    Ok(())
}

fn snapshot() -> Result<()> {
    println!("== GPU ==");
    match sensors::gpu::GpuSensor::new() {
        Ok(gpu) => match gpu.info().and_then(|info| Ok((info, gpu.read()?))) {
            Ok((info, s)) => print_gpu(&info, &s),
            Err(e) => println!("GPU found but reading sensors failed: {e:#}"),
        },
        Err(_) => println!("GPU: unavailable (no NVIDIA driver/GPU detected on this machine)"),
    }

    println!();
    println!("== CPU / RAM ==");
    let mut cpu = sensors::cpu::CpuSensor::new();
    std::thread::sleep(sensors::cpu::MIN_SAMPLE_GAP);
    print_cpu(&cpu.read());

    let cfg = config::load_or_create()?;
    match sensors::cpu_temp::detect(&cfg.lhm_url) {
        Some(mut source) => match source.read_c() {
            Ok(t) => println!("CPU temp:     {t:.1} C ({})", source.label()),
            Err(e) => println!("CPU temp:     unavailable ({e:#})"),
        },
        None => println!("CPU temp:     unavailable"),
    }
    Ok(())
}

fn print_gpu(info: &sensors::gpu::GpuInfo, s: &sensors::gpu::GpuSample) {
    println!("Name:         {}", info.name);
    println!("Temperature:  {} C", s.temp_c);
    println!("Core clock:   {} MHz", s.core_mhz);
    println!("Memory clock: {} MHz", s.mem_mhz);
    println!(
        "Utilisation:  GPU {}%  /  memory bus {}%",
        s.util_pct, s.mem_util_pct
    );
    println!("Power draw:   {:.1} W", s.power_w);
    println!(
        "VRAM:         {} / {} MiB",
        s.vram_used_mib, info.vram_total_mib
    );
    if s.throttle.is_empty() {
        println!("Throttling:   none");
    } else {
        println!("Throttling:   {}", s.throttle.join(", "));
    }
}

fn print_cpu(s: &sensors::cpu::CpuSnapshot) {
    println!("Name:         {}", s.name);
    println!("Usage:        {:.1}%", s.sample.usage_pct);
    if !s.live_clocks {
        println!("(live clock counters unavailable; showing static base clock)");
    }
    for (i, (mhz, usage)) in s
        .sample
        .per_core_mhz
        .iter()
        .zip(&s.sample.per_core_usage)
        .enumerate()
    {
        println!("  core {i:>2}:    {mhz:>5} MHz  {usage:>5.1}%");
    }
    println!("RAM:          {} / {} MiB", s.ram_used_mib, s.ram_total_mib);
}
