# heatlog

[![CI](https://github.com/Hami-K/heatlog/actions/workflows/ci.yml/badge.svg)](https://github.com/Hami-K/heatlog/actions/workflows/ci.yml)

A Windows app that records what your hardware is doing while you game, then
tells you in plain language whether it throttled, when, and why. It watches
the GPU - NVIDIA, Intel or AMD, including the integrated one beside a
dedicated card - plus CPU usage, clocks, temperature, memory and fan
speeds.

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

![The heatlog desktop app's Overview page: "Running cool", gauges for CPU
usage, memory, GPU usage, GPU temperature and CPU temperature, and a
60-second activity graph](docs/overview.png)

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

## Graphics cards

heatlog reads whatever GPU the machine has, through whichever interface
reports the most about it:

- **NVIDIA** through NVML, NVIDIA's own management library — the same source
  behind `nvidia-smi`. This reports temperature, clocks, power draw and, most
  usefully, the driver's own **throttle reasons**, so heatlog can say *why*
  the GPU slowed down rather than guessing.
- **Intel and AMD, integrated or dedicated**, through Windows' own graphics
  interfaces: the performance counters behind Task Manager's GPU tab for
  utilisation and memory, and the kernel-mode thunk interface for the adapter
  itself. No driver, no admin rights.

What the second route cannot do is worth stating plainly: Windows exposes no
clock speed, no power draw and **no throttle signal** for these GPUs, and
most integrated GPUs report no temperature either. heatlog shows what it has
and says "not reported" for the rest — it never prints a zero in place of a
missing reading. Crucially, a report on such a GPU says that throttling could
not be checked, instead of claiming there was none.

On a laptop with **both** an integrated and a dedicated GPU, both are
detected. The dedicated one is the main GPU (read through NVML where it is an
NVIDIA card) and the integrated one appears beneath it on the GPU page, with
its own figures; the two are never added together, and the same GPU is never
listed twice.

**Known issue on non-NVIDIA GPUs**: the usage trend is reliable - when it
rises, the GPU is busier - but the exact percentage can differ from the one
Task Manager shows, and heatlog sometimes reads higher. It is being looked at
for a later version.

**Dedicated AMD GPUs are experimental**: that combination has never run on
real hardware, so heatlog labels it as untested in the app rather than
implying otherwise. Integrated AMD and Intel graphics are tested and part of
this release.

## CPU temperature and fan speeds

Windows has no unprivileged API for CPU temperature or fan speed. heatlog
can read the temperature from the CPU itself through the PawnIO driver, or
borrow it from a monitoring app that already ships a signed driver. It looks
for these in order, and keeps working without any of them:

1. **PawnIO** (recommended), which reads the temperature the CPU reports
   about itself - no other program has to be running. Install it from the
   app's setup page or with `winget install --id namazso.PawnIO -e`; Windows
   asks for admin permission once, because it installs a driver.

   **heatlog has to be running as administrator to use it.** PawnIO's device
   is open to administrators alone - that is set in PawnIO's own installer,
   deliberately, because the driver can read any machine register - so an
   ordinary heatlog run cannot reach it. heatlog never asks you to elevate
   and never installs a privileged helper of its own: start it normally and
   it uses one of the apps below; right-click it and choose **Run as
   administrator** and it reads the CPU directly. Nothing is ever written:
   heatlog loads PawnIO's own signed Intel and AMD modules and reads
   temperature registers, and no module that can write anything is loaded.
   Fan speeds still come from the apps below, since PawnIO's official
   modules have nothing for laptop fans.
2. **LibreHardwareMonitor** through its web server
   (`winget install LibreHardwareMonitor.LibreHardwareMonitor`, then
   Options > Remote Web Server > Run). The package is portable: it adds no
   Start menu entry and must run as administrator, so heatlog finds it and
   offers to start it for you.
3. **HWiNFO** through its shared-memory interface. Install it with
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

You can pin one source in Settings (or `temp_source` in the config file):
`auto` uses the best one available, or name `pawnio`,
`librehardwaremonitor` or `hwinfo` and heatlog uses only that one.

The app's Settings page has an **Open setup** button that detects which of
these is installed or running, installs PawnIO or HWiNFO through winget,
offers to start LibreHardwareMonitor, and re-checks - by itself, every few
seconds, while that page is open, so nothing ever needs a restart. heatlog never
changes another app's settings for you. If Windows Firewall asks about
LibreHardwareMonitor's network access, blocking it is fine: heatlog only
reads from this PC.

An NVIDIA GPU needs none of this: NVIDIA's own library reports its
temperature directly. Intel and AMD GPUs usually report none at all - see
above.

## Is that driver still safe?

A kernel driver that is safe today can become a liability tomorrow: a hole
is found, Microsoft adds it to the vulnerable-driver blocklist, or the
version on your PC is simply left behind. heatlog therefore checks before it
reads anything through PawnIO, and the checks lean on sources other people
keep up to date - they have to keep working even if heatlog itself stops
being maintained:

- **Windows.** A blocklisted driver does not load, and heatlog notices that
  it cannot open the device and says so in plain words.
- **Published advisories.** heatlog searches the NVD's CVE database for
  advisories naming PawnIO. Any match disables the driver until the advisory
  is ruled out in the list below. (GitHub's advisory database cannot help
  here: it is indexed by software-package ecosystem, and a Windows driver
  belongs to none of them.)
- **PawnIO's own releases.** A newer version gets a one-click
  **Update PawnIO** button.
- **A dead man's switch.** If heatlog's own newest release is more than a
  year old, heatlog assumes nobody is looking after it, stops using PawnIO
  and says why - LibreHardwareMonitor is maintained and keeps PawnIO up to
  date. A setting takes that decision back.
- **[`driver-safety.json`](driver-safety.json)** in this repository, read
  from a URL that will never change. It lists blocked versions with a reason
  each, the oldest version heatlog will talk to, and advisories already
  ruled out. Blocking a version needs an edit to that file, not a release.
  Its `format` number guards it: a heatlog that does not recognise the
  format stops using PawnIO rather than guessing.

Every answer fails safe. Anything blocked, unknown, unreadable or never yet
checked means heatlog falls back to LibreHardwareMonitor or HWiNFO, with the
reason on the CPU page and the setup page, and only the PawnIO source is
ever switched off - heatlog itself keeps running and recording. An offline
PC keeps using the last answer for a day. This is not the update check:
turning update checks off leaves the safety check on, and turning the safety
check off means heatlog never uses PawnIO at all.

## Configuration

Settings live in `%APPDATA%\heatlog\config.toml`, created on first run, and
can be edited from the app's Settings page:

- `interval_secs` - seconds between samples (default 1)
- `sessions_dir` - where sessions are stored; defaults to
  `%APPDATA%\heatlog\sessions`
- `games` - process names for auto-record (find the exact name in Task
  Manager's Details tab while the game is running)
- `lhm_url` - LibreHardwareMonitor web server endpoint
- `temp_source` - where the CPU temperature comes from: `auto`, or one of
  `pawnio`, `librehardwaremonitor`, `hwinfo` to use only that one
- `pawnio_safety_check` - check Windows, published advisories and heatlog's
  blocked-version list before reading hardware through the PawnIO driver
  (default true; off means heatlog never uses PawnIO)
- `pawnio_if_heatlog_unmaintained` - keep using PawnIO even when heatlog
  itself has had no release for over a year (default false)
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

Recordings stopped within about ten seconds, or with fewer than two
samples, are discarded rather than saved: there is nothing in them to chart
or judge, and the app says so.

A one-hour session is roughly 2 MB.

## Updates

On start-up the app asks GitHub whether a newer release exists. If several
have come out since, it offers the newest one, not the next one in line.

The update happens inside heatlog: a dialog downloads that release's
program files, checks each one against the SHA-256 published in the same
release, and replaces heatlog's own files in place. There is no installer,
no setup wizard and no admin prompt, because everything lives under your
own `%LOCALAPPDATA%`. A download that does not match its checksum, or that
is not listed at all, is refused with an explanation rather than installed,
and downloads only ever come from this project's own release URLs over
HTTPS. If any step fails the previous files are put back, so heatlog always
still launches. Windows cannot overwrite a running program, so the old one
is renamed aside and deleted the next time heatlog starts.

Updates never run while a recording is running, and the new files land at
exactly the same paths under the same names, so taskbar and Start menu pins
keep working. The release's `heatlog-setup.exe` is only for a first
install.

Those requests are the only time heatlog contacts the internet, and they
send nothing but the request itself. Turn them off with the "Check GitHub
for updates on start-up" checkbox in Settings, or `check_updates = false`
in the config file. Pre-releases are ignored unless you opt in.

After an update installs, the next start shows once what changed since the
version you were on, combining the notes of every release you skipped.

## Overhead

Measured while monitoring at one sample per second, on a release build
running on a 20-thread laptop CPU under Windows 11:

| | CPU | Memory |
|---|---|---|
| `heatlog record` (command line) | 0.60% of one core | 19 MB |
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

Reading a GPU through Windows' performance counters is most of that first
row: it was 0.28% of a core before that path existed, on a machine where the
only GPU is an integrated one. An NVIDIA GPU is cheaper to read, because NVML
answers directly rather than through a counter query.

Those rows were measured with no CPU temperature source set up. Reading the
temperature costs something whichever way it is read, and reading it from
the CPU is the cheaper way: measured on the same laptop at one sample a
second, the recorder spent 2.1% of a core through PawnIO and 2.5% through
LibreHardwareMonitor's web server. An HTTP request per sample is simply more
work than a register read. Both figures include reading each core's own
register every second; the shipped build does that once every ten samples
instead, because only the package reading is recorded, so its cost sits
below the 2.1% measured here.

The WMI provider process that serves the performance-counter queries
(WmiPrvSE) showed no measurable CPU increase.

## How it works

- **GPU** - NVIDIA through NVML (the library behind `nvidia-smi`):
  temperature, core/memory clocks, utilisation, power draw, VRAM, and the
  driver's own throttle reasons (thermal limit, power cap, ...). Intel and
  AMD through Windows' GPU performance counters and `D3DKMT` adapter
  queries: utilisation and memory, matched to each adapter by its LUID the
  way Task Manager does it, and whatever else the driver fills in.
- **CPU clocks** - Windows' "Processor Information" performance counters, per
  core: percent-of-base-clock times each core's base frequency, the same
  calculation Task Manager shows. CPU usage and RAM come from the same
  counters via the `sysinfo` crate.
- **CPU temperature and fans** - the CPU's own registers through the PawnIO
  driver when heatlog is elevated and the driver has been checked
  (`IA32_PACKAGE_THERM_STATUS` and `IA32_THERM_STATUS` against Tjmax on
  Intel, Tctl on AMD), otherwise LibreHardwareMonitor's web server or
  HWiNFO's shared memory, as described above. Fans always come from one of
  the apps. Degraded to "unavailable" rather than guessed.
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
