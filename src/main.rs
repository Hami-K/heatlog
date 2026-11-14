mod sensors;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Snapshot => snapshot(),
    }
}

fn snapshot() -> Result<()> {
    println!("== GPU ==");
    match sensors::gpu::GpuSensor::new() {
        Ok(gpu) => match gpu.read() {
            Ok(s) => print_gpu(&s),
            Err(e) => println!("GPU found but reading sensors failed: {e:#}"),
        },
        Err(_) => println!("GPU: unavailable (no NVIDIA driver/GPU detected on this machine)"),
    }

    println!();
    println!("== CPU / RAM ==");
    let mut cpu = sensors::cpu::CpuSensor::new();
    std::thread::sleep(sensors::cpu::MIN_SAMPLE_GAP);
    print_cpu(&cpu.read());
    Ok(())
}

fn print_gpu(s: &sensors::gpu::GpuSnapshot) {
    println!("Name:         {}", s.name);
    println!("Temperature:  {} C", s.temp_c);
    println!("Core clock:   {} MHz", s.core_clock_mhz);
    println!("Memory clock: {} MHz", s.mem_clock_mhz);
    println!(
        "Utilisation:  GPU {}%  /  memory bus {}%",
        s.gpu_util_pct, s.mem_util_pct
    );
    println!("Power draw:   {:.1} W", s.power_w);
    println!(
        "VRAM:         {} / {} MiB",
        s.vram_used_mib, s.vram_total_mib
    );
    if s.throttle_reasons.is_empty() {
        println!("Throttling:   none");
    } else {
        println!("Throttling:   {}", s.throttle_reasons.join(", "));
    }
}

fn print_cpu(s: &sensors::cpu::CpuSnapshot) {
    println!("Name:         {}", s.name);
    println!("Usage:        {:.1}%", s.usage_pct);
    if !s.live_clocks {
        println!("(live clock counters unavailable; showing static base clock)");
    }
    for (i, (mhz, usage)) in s.per_core_mhz.iter().zip(&s.per_core_usage).enumerate() {
        println!("  core {i:>2}:    {mhz:>5} MHz  {usage:>5.1}%");
    }
    println!("RAM:          {} / {} MiB", s.ram_used_mib, s.ram_total_mib);
}
