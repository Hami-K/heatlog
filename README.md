# heatlog

[![CI](https://github.com/Hami-K/heatlog/actions/workflows/ci.yml/badge.svg)](https://github.com/Hami-K/heatlog/actions/workflows/ci.yml)

A Windows command-line tool that records hardware stats while you game and
summarises them afterwards: peak temperatures and clocks, how long the GPU
throttled and why, and when the worst moments happened.

Monitoring is read-only. heatlog talks to hardware sensors and the process
list only — it never touches game processes, injects code, or draws overlays,
and it does not need administrator rights.

## Requirements

- Windows 10/11
- Rust toolchain (MSVC) to build
- NVIDIA GPU — optional; without one, CPU and RAM are still recorded
- [LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor)
  with its "Remote Web Server" enabled — optional, for CPU temperature.
  Without it heatlog tries the ACPI thermal zone, and otherwise marks CPU
  temperature as unavailable.

## Install

Download `heatlog.exe` from the
[latest release](https://github.com/Hami-K/heatlog/releases) and run it from a
terminal, or build from source:

```
git clone https://github.com/Hami-K/heatlog
cd heatlog
cargo build --release
```

The binary is `target\release\heatlog.exe`.

## Usage

```
heatlog snapshot          # read all sensors once and print them
heatlog record            # record until stopped (q, Esc or Ctrl-C)
heatlog record --auto     # wait for a configured game, record while it runs
heatlog record --interval 2 --out D:\logs --no-tui   # optional overrides
heatlog list              # show recorded sessions
heatlog report            # summarise the most recent session
heatlog report 2026-11-14_23-34-05          # summarise a specific one
heatlog report --html --open                # HTML report with charts
```

While recording, heatlog shows a live dashboard with current values and
sparkline history for GPU temperature, GPU utilisation, CPU usage and the
fastest core's clock, plus a throttle indicator. `--no-tui` (or piping the
output) switches to plain line-by-line status instead.

Example report:

```
Session 2026-11-14 23:34:05
Duration 0:20  (20 samples @ 1s)

GPU  NVIDIA GeForce RTX 4060 Laptop GPU
  Temperature  avg 74 C, peak 84 C at 0:12
  Core clock   avg 2370 MHz, peak 2550 MHz at 0:03
  Throttling
    thermal limit (software): 2 episode(s), 0:07 total, longest at 0:10
...
```

`report --html` writes a single self-contained HTML file next to the session
(everything is inline; nothing is fetched from the internet). It opens with a
colour-coded plain-language verdict ("Your GPU slowed itself down 2 times to
avoid overheating, mostly around minute 33"), headline numbers in friendly
units, then time charts with hover tooltips for GPU temperature, speed and
power, and CPU usage, speed and temperature. Shaded regions mark GPU
throttling (driver-reported) and estimated CPU slowdowns, temperature charts
show the hot zone, and the full technical numbers (peaks, timestamps,
per-reason breakdown) sit in a collapsible section. Long sessions are
downsampled for display in a way that preserves spikes. `--open` launches the
file in the default browser.

<!-- SCREENSHOTS: add terminal screenshots here -->

## Desktop app

`heatlog-gui` is a desktop app laid out like a modern Windows system tool,
with a warm "heat" theme: temperature-related visuals run a cool-blue to
amber to rust-red scale, so colour itself shows how hot things are.

- live monitoring of CPU, GPU and memory (60-second graphs, stats, and a
  sidebar with mini graphs and current values), updating every second
- an Overview dashboard with round heat-coloured gauges and a plain-language
  health status ("Running cool" / "Getting warm" / "Too hot — slowing down",
  or a neutral note when no temperature source exists)
- one-click recording with a running timer, plus auto-record that starts
  when a configured game launches and stops when it exits
- a Recordings page listing past sessions with verdict pills and peak
  temperatures; opening one shows the verdict, charts with point-snapping
  tooltips and throttle shading, technical details, and an Export HTML button

Build and run it from source:

```
cargo run --release --bin heatlog-gui
```

Settings (the game list for auto-record, the recording interval) are read
from the config file described below; in-app editing is planned.

## Configuration

`heatlog record` creates `%APPDATA%\heatlog\config.toml` on first run, with
comments explaining each option:

- `interval_secs` — seconds between samples (default 1)
- `sessions_dir` — where sessions are stored; defaults to
  `%APPDATA%\heatlog\sessions`
- `games` — process names for `--auto` (find the exact name in Task
  Manager's Details tab while the game is running)
- `lhm_url` — LibreHardwareMonitor web server endpoint for CPU temperature

Sessions are JSON Lines files (`.jsonl`), one metadata line followed by one
JSON object per sample, so a crash or kill loses at most the last sample.

## Overhead

Measured while recording at 1-second intervals (release build, Intel Core
i9-13900H, Windows 11): about 0.14% of one CPU core and 20 MB of memory for
heatlog itself, with no measurable CPU increase in the WMI provider process
(WmiPrvSE) that serves the performance-counter queries.

## How it works

- **GPU** — queried through NVML (the library behind `nvidia-smi`):
  temperature, core/memory clocks, utilisation, power draw, VRAM, and the
  driver's own throttle reasons (thermal limit, power cap, ...).
- **CPU clocks** — Windows' "Processor Information" performance counters,
  per core: percent-of-base-clock times each core's base frequency, the same
  calculation Task Manager shows. CPU usage and RAM come from the same
  counters via the `sysinfo` crate.
- **CPU temperature** — Windows has no reliable unprivileged API, so heatlog
  probes LibreHardwareMonitor's local web server, falls back to the ACPI
  thermal zone, and degrades to "unavailable" rather than failing.
- **Reports** — `report` computes averages, peaks with timestamps, and groups
  consecutive throttled samples into episodes per reason. CPU throttling is a
  clearly-labelled heuristic (sustained high load with clocks well below the
  session's own loaded baseline), because Windows exposes no direct signal.

## License

MIT — see [LICENSE](LICENSE).
