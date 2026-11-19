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
    // Left behind by an update: a running program cannot delete itself.
    heatlog::update::clean_old_files();
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
    print!("Download and install it now? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("Left alone.");
        return Ok(());
    }

    // heatlog replaces its own files, so there is no installer to run and
    // no admin prompt; the running exe is renamed aside and removed on the
    // next start.
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut shown = 0u64;
    heatlog::update::install_in_place(&available, &cancel, |progress| {
        match progress {
            heatlog::update::Progress::Downloading { got, total } => {
                // One line per megabyte, not per chunk.
                if got >= shown + (1 << 20) || got == total {
                    shown = got;
                    println!("  {} MB of {} MB", got >> 20, total >> 20);
                }
            }
            heatlog::update::Progress::Installing => println!("Installing..."),
        }
    })?;
    println!(
        "heatlog {} installed. Run it again to use the new version.",
        available.version
    );
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
    let mut gpus = sensors::gpu::detect();
    if gpus.is_empty() {
        println!("GPU: unavailable (no GPU heatlog can read on this machine)");
    }
    for (i, gpu) in gpus.iter_mut().enumerate() {
        if i > 0 {
            println!();
        }
        let info = gpu.info().clone();
        match gpu.read() {
            Ok(sample) => print_gpu(&info, &sample),
            Err(e) => println!("{}: reading sensors failed: {e:#}", info.name),
        }
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
    let mut tags = Vec::new();
    if info.integrated {
        tags.push("integrated");
    }
    if info.experimental {
        tags.push("experimental");
    }
    println!(
        "Name:         {}{}",
        info.name,
        if tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", tags.join(", "))
        }
    );
    println!("Read through: {}", info.source);
    // Only what this GPU reports; the rest says so rather than printing 0.
    match s.temp_c {
        Some(t) => println!("Temperature:  {t} \u{00b0}C"),
        None => println!("Temperature:  not reported"),
    }
    if let Some(mhz) = s.core_mhz {
        println!("Core clock:   {mhz} MHz");
    }
    if let Some(mhz) = s.mem_mhz {
        println!("Memory clock: {mhz} MHz");
    }
    match s.mem_util_pct {
        Some(mem) => println!("Utilisation:  GPU {}%  /  memory bus {mem}%", s.util_pct),
        None => println!("Utilisation:  GPU {}%", s.util_pct),
    }
    match s.power_w {
        Some(w) => println!("Power draw:   {w:.1} W"),
        None => println!("Power draw:   not reported"),
    }
    match (info.vram_total_mib, s.shared_mem_used_mib) {
        (Some(total), _) => println!("VRAM:         {} / {total} MiB", s.vram_used_mib),
        (None, Some(shared)) => println!(
            "Memory:       {} MiB dedicated + {shared} MiB shared",
            s.vram_used_mib
        ),
        (None, None) => println!("Memory:       {} MiB", s.vram_used_mib),
    }
    match &s.throttle {
        None => println!("Throttling:   not reported by this GPU"),
        Some(reasons) if reasons.is_empty() => println!("Throttling:   none"),
        Some(reasons) => println!("Throttling:   {}", reasons.join(", ")),
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
