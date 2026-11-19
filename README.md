# heatlog

[![CI](https://github.com/Hami-K/heatlog/actions/workflows/ci.yml/badge.svg)](https://github.com/Hami-K/heatlog/actions/workflows/ci.yml)

A Windows app that records what your hardware is doing while you game, then
tells you in plain language whether it throttled, when, and why. It watches
GPU temperature, clocks, power and the driver's own throttle reasons, plus
CPU usage, clocks and memory.

Monitoring is read-only. heatlog talks to hardware sensors and the process
list only: it never touches game processes, injects code, or draws overlays,
and it does not need administrator rights.

## Download

Get **heatlog-setup.exe** from the
[latest release](https://github.com/Hami-K/heatlog/releases/latest) and run
it. It installs for your user account only (under `%LOCALAPPDATA%\Programs`),
adds a Start menu shortcut, and registers an uninstaller in Windows
"Installed apps". No admin prompt.

Windows SmartScreen may warn that the publisher is unknown, because the
installer is not code-signed (a certificate costs money this project does not
have). Choose "More info" then "Run anyway" if you are happy to.

The release also has **heatlog-gui.exe** (the app) and **heatlog.exe** (the
command line tool) as portable downloads that run without installing, and
**SHA256SUMS.txt** to check any of them against:

```
(Get-FileHash .\heatlog-setup.exe -Algorithm SHA256).Hash
```

## Desktop app

The app is laid out like a modern Windows system tool, with a warm "heat"
theme: temperature visuals run a cool-blue to amber to rust-red scale, so
colour itself shows how hot things are.

- Live monitoring of CPU, GPU and memory: 60-second graphs, stat grids, and a
  sidebar with mini graphs and current values, updating every second
- An Overview dashboard with heat-coloured gauges and a plain-language health
  status ("Running cool", "Getting warm", "Too hot - slowing down")
- One-click recording with a running timer, plus auto-record that starts when
  a configured game launches and stops when it exits (Ctrl+R also toggles)
- A Recordings page listing past sessions with verdict pills, peak
  temperatures and a sparkline preview; opening one shows the verdict,
  possible causes for its slow moments, charts with point-snapping
  tooltips, technical details, and an Export HTML button

## Command line

```
heatlog snapshot          # read all sensors once and print them
heatlog record            # record until stopped (q, Esc or Ctrl-C)
heatlog record --auto     # wait for a configured game, record while it runs
heatlog record --interval 2 --out D:\logs --no-tui   # optional overrides
heatlog list              # show recorded sessions
heatlog report            # summarise the most recent session
heatlog report 2026-11-14_23-34-05          # summarise a specific one
heatlog report --html --open                # HTML report with charts
heatlog update            # check GitHub for a newer version
```

While recording, the command line tool shows a live dashboard with sparkline
history. `--no-tui` (or piping the output) switches to plain status lines.

`report --html` writes a single self-contained HTML file next to the session
(everything is inline; nothing is fetched from the internet). It opens with a
colour-coded plain-language verdict ("Your GPU slowed itself down 2 times to
avoid overheating, mostly around minute 33"), headline numbers in friendly
units, then time charts with hover tooltips for GPU temperature, speed and
power, and CPU usage, speed and temperature. Shaded regions mark GPU
throttling (driver-reported) and estimated CPU slowdowns, temperature charts
show the hot zone, and the full technical numbers sit in a collapsible
section. Long sessions are downsampled in a way that preserves spikes.

<!-- SCREENSHOTS: add app and report screenshots here -->

## CPU temperature and fan speeds

Windows has no unprivileged API for CPU temperature or fan speed, so heatlog
borrows them from a monitoring app that already ships a signed driver. It
looks for these in order, and keeps working without either:

1. **LibreHardwareMonitor** (recommended) through its web server
   (`winget install LibreHardwareMonitor.LibreHardwareMonitor`, then
   Options > Remote Web Server > Run). The package is portable: it adds no
   Start menu entry and must run as administrator, so heatlog finds it and
   offers to start it for you.
2. **HWiNFO** through its shared-memory interface. Install it with
   `winget install --id REALiX.HWiNFO -e`, or from
   [hwinfo.com](https://www.hwinfo.com/download/) if winget's download
   fails; start it in sensors-only mode, tick **Shared Memory Support** in
   its settings, and leave it running. The free version switches that option
   off again after 12 hours; heatlog says so when it happens.

Temperature and fan speed are sourced separately, because the two apps see
different things. LibreHardwareMonitor reads fans off a Super I/O chip,
which most laptops do not have, so on a laptop it often reports a CPU
temperature and no fans at all; HWiNFO asks the embedded controller and
usually does report them. heatlog takes the temperature from the first
source that has one and the fan speeds from whichever source has those,
even if that is the other app. When neither does, the CPU page says so
instead of showing nothing.

The app's Settings page has an **Open setup** button that detects which of
these is installed or running, installs HWiNFO through winget, offers to
start LibreHardwareMonitor, and re-checks - by itself, every few seconds,
while that page is open, so nothing ever needs a restart. heatlog never
changes another app's settings for you. If Windows Firewall asks about
LibreHardwareMonitor's network access, blocking it is fine: heatlog only
reads from this PC.

GPU temperature needs none of this: NVIDIA's own management library reports
it directly.

## Configuration

Settings live in `%APPDATA%\heatlog\config.toml`, created on first run, and
can be edited from the app's Settings page:

- `interval_secs` - seconds between samples (default 1)
- `sessions_dir` - where sessions are stored; defaults to
  `%APPDATA%\heatlog\sessions`
- `games` - process names for auto-record (find the exact name in Task
  Manager's Details tab while the game is running)
- `lhm_url` - LibreHardwareMonitor web server endpoint
- `check_updates`, `include_prereleases` - update checking (see below)
- `delete_after_days` - delete recordings older than this on start-up; 0
  keeps them forever
- `record_processes` - record which programs were busy, so reports can
  suggest what caused a slowdown (see below)

Sessions are JSON Lines files (`.jsonl`), one metadata line followed by one
JSON object per sample, so a crash or kill loses at most the last sample.

## Possible causes, and your privacy

When a recording contains slow moments, the report suggests what might
explain them: heat, a power limit, the cooling system already at its limit,
a background program that was busy at the time, or memory running out.
These are read off the recording, so they are labelled as possibilities
rather than findings.

To name a background program, heatlog records the busiest few processes
every few seconds. That means program names are written into the recording
files **on this PC**, and nothing is ever sent anywhere. If you would rather
record hardware readings only, untick **Record which programs were busy**
in Settings, or set `record_processes = false` in the config file. Reports
then skip the background-program suggestion and keep the rest.

heatlog only ever reads the process list, which Windows shows to any
program. It never looks inside another process.

## Fans and cooling limits

When a monitoring app reports fan speeds, heatlog records them too, charts
them, and uses them to tell two situations apart: the fans were already at
full speed while the hardware throttled, which means the machine is cooling
as hard as it can, or the hardware throttled while the fans still had room,
which points at a fan curve or power plan and the laptop maker's own
utility. heatlog never controls fans.

## Recordings and disk space

The Settings page shows how many recordings there are and how much disk space
they use. Recordings can be deleted one at a time or all at once, both behind
a confirmation, and optionally pruned automatically after a number of days.
Deleted recordings (and any HTML report exported beside them) go to the
Recycle Bin; if Windows refuses, heatlog deletes them outright instead.

A one-hour session is roughly 2 MB.

## Updates

On start-up the app asks GitHub whether a newer release exists, and offers a
one-click update that downloads the installer, checks it against the
SHA-256 published in that release and only then runs it. A download that
does not match its checksum, or that is not listed at all, is refused with
an explanation rather than run, and downloads only ever come from this
project's own release URLs over HTTPS. That request is the
only time heatlog contacts the internet, it sends nothing but the request
itself, and it never updates while a recording is running. Turn it off with
the "Check GitHub for updates on start-up" checkbox in Settings, or
`check_updates = false` in the config file. Pre-releases are ignored unless
you opt in.

## Overhead

Measured while monitoring at one sample per second, on a release build
running on a 20-thread laptop CPU under Windows 11:

| | CPU | Memory |
|---|---|---|
| `heatlog record` (command line) | 0.28% of one core | 19 MB |
| the same, also recording which programs are busy | 0.94% of one core | 20 MB |
| app in the background, which is where it sits while you game | 2.0% of one core | 145 MB |
| app in the foreground with the window visible | 3.4% of one core | 145 MB |

The app draws once a second, when a new sample arrives. It only redraws
faster while the window has focus (to let numbers glide), and not at all
while it is minimised. Resting the mouse pointer on a chart costs about 7%
of a core while it sits there, because the readout follows the pointer.

Recording which programs are busy is the one expensive thing heatlog can
do: asking Windows about every process costs around 4% of a core each time,
so heatlog does it every few seconds rather than every second, which is
plenty for explaining a slow stretch. Turning it off in Settings returns the
recorder to the first row.

The WMI provider process that serves the performance-counter queries
(WmiPrvSE) showed no measurable CPU increase.

## How it works

- **GPU** - queried through NVML (the library behind `nvidia-smi`):
  temperature, core/memory clocks, utilisation, power draw, VRAM, and the
  driver's own throttle reasons (thermal limit, power cap, ...).
- **CPU clocks** - Windows' "Processor Information" performance counters, per
  core: percent-of-base-clock times each core's base frequency, the same
  calculation Task Manager shows. CPU usage and RAM come from the same
  counters via the `sysinfo` crate.
- **CPU temperature and fans** - read from LibreHardwareMonitor's web
  server or HWiNFO's shared memory, as described above, and degraded to
  "unavailable" rather than guessed.
- **Possible causes** - the busiest processes during a session's slow
  moments, combined with what the driver reported, turned into plain
  suggestions. Process names never leave the PC.
- **Reports** - averages, peaks with timestamps, and consecutive throttled
  samples grouped into episodes per reason. CPU throttling is a clearly
  labelled estimate (sustained high load with clocks well below the session's
  own loaded baseline), because Windows exposes no direct signal.

## Build from source

Requires the Rust toolchain (MSVC) and Windows 10/11:

```
git clone https://github.com/Hami-K/heatlog
cd heatlog
cargo build --release
```

The binaries are `target\release\heatlog-gui.exe` and
`target\release\heatlog.exe`. Building the installer additionally needs
[Inno Setup 6](https://jrsoftware.org/isinfo.php):

```
ISCC.exe /DAppVersion=0.2.0 installer\heatlog.iss
```

## License

MIT - see [LICENSE](LICENSE).
