mod record;
mod tui;

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
        /// Plain line-by-line output instead of the live dashboard
        #[arg(long)]
        no_tui: bool,
    },
    /// Summarise a recorded session (defaults to the most recent)
    Report {
        /// Session file: a path, or a name from `heatlog list`
        session: Option<String>,
        /// Write a self-contained HTML report with charts next to the session
        #[arg(long)]
        html: bool,
        /// Open the HTML report in the default browser (implies --html)
        #[arg(long)]
        open: bool,
    },
    /// Show recorded sessions
    List,
    /// Check GitHub for a newer heatlog and install it after confirmation
    Update,
}

fn main() -> Result<()> {
    // Degree signs and the dashboard's box drawing are UTF-8; a console
    // left on a legacy code page would print them as mojibake.
    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleOutputCP(65001);
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Snapshot => snapshot(),
        Command::Record {
            auto,
            interval,
            out,
            no_tui,
        } => {
            let cfg = config::load_or_create()?;
            record::run(&cfg, auto, interval, out, no_tui)
        }
        Command::Report {
            session,
            html,
            open,
        } => {
            let cfg = config::load_or_create()?;
            let path = resolve_session(&cfg, session.as_deref())?;
            let session = session::read(&path)?;
            let summary = report::analyze(&session)?;
            if html || open {
                let out = path.with_extension("html");
                std::fs::write(&out, report::html::render(&session, &summary)?)?;
                println!("Wrote {}", out.display());
                if open {
                    // `start` must go through cmd; the empty "" is its window title.
                    std::process::Command::new("cmd")
                        .args(["/C", "start", ""])
                        .arg(&out)
                        .spawn()?;
                }
            } else {
                report::print(&summary);
                report::print_causes(&report::causes::causes(&session, &summary));
            }
            Ok(())
        }
        Command::Update => update(),
        Command::List => {
            let cfg = config::load_or_create()?;
            list_sessions(&cfg)
        }
    }
}

fn update() -> Result<()> {
    let cfg = config::load_or_create()?;
    println!("This version: {}", heatlog::update::current_version());
    let Some(available) = heatlog::update::check(cfg.include_prereleases)? else {
        println!("heatlog is up to date.");
        return Ok(());
    };
    let kind = if available.prerelease {
        " (pre-release)"
    } else {
        ""
    };
    println!("Update available: {}{kind}", available.version);
    let Some(installer) = &available.installer else {
        println!("No installer in that release. See {}", available.page_url);
        return Ok(());
    };

    print!("Download and run the installer now? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("Left alone.");
        return Ok(());
    }
    println!(
        "Downloading {} and checking its published SHA-256...",
        installer.name
    );
    let path = heatlog::update::run_installer(&available)?;
    println!("Installer started from {}", path.display());
    Ok(())
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
        match session::peek(&path) {
            Ok(s) => {
                let mins = s.duration_secs / 60;
                let secs = s.duration_secs % 60;
                println!(
                    "  {}  {mins}:{secs:02}  {} samples  {}",
                    s.meta.started.format("%Y-%m-%d %H:%M:%S"),
                    s.sample_count,
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
        Some(mut source) => match source.read() {
            Ok(reading) => {
                match reading.cpu_temp_c {
                    Some(t) => println!("CPU temp:     {t:.1} °C ({})", source.label()),
                    None => println!("CPU temp:     unavailable"),
                }
                for fan in &reading.fans {
                    println!("Fan:          {} rpm ({})", fan.rpm, fan.name);
                }
            }
            Err(e) => println!("CPU temp:     unavailable ({e:#})"),
        },
        None => println!("CPU temp:     unavailable"),
    }
    Ok(())
}

fn print_gpu(info: &sensors::gpu::GpuInfo, s: &sensors::gpu::GpuSample) {
    println!("Name:         {}", info.name);
    println!("Temperature:  {} °C", s.temp_c);
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
