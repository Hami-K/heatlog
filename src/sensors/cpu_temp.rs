use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sysinfo::{ProcessesToUpdate, System};

use super::hwinfo::HwInfoShm;
use super::pawnio::{self, OpenError};
use crate::config::Config;
use crate::driver_safety;

/// One fan, as the monitoring app names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fan {
    pub name: String,
    pub rpm: u32,
}

/// What a monitoring app can tell us that Windows will not: the CPU
/// package temperature, and how hard the fans are working.
#[derive(Debug, Default)]
pub struct Reading {
    pub cpu_temp_c: Option<f32>,
    /// Per-core temperatures, hottest last, when the source reads the
    /// cores one by one (PawnIO does; the monitoring apps hand over one
    /// package figure). Not recorded into sessions - they are a live
    /// detail, and a session file should not grow by a column per core.
    pub cores: Vec<f32>,
    pub fans: Vec<Fan>,
}

impl Reading {
    pub fn fastest_fan(&self) -> Option<u32> {
        self.fans.iter().map(|fan| fan.rpm).max()
    }
}

/// Windows gives no unprivileged API for CPU temperature or fan speed, so
/// heatlog borrows them from a monitoring app that already has a signed
/// driver. Sources are probed in order and the whole feature degrades to
/// "unavailable" rather than failing.
pub trait CpuTempSource {
    fn label(&self) -> &'static str;
    fn read(&mut self) -> Result<Reading>;
}

/// Returns the first source that produces a plausible reading right now.
///
/// PawnIO leads: it reads the CPU's own temperature register instead of
/// borrowing another app's reading, so nothing else has to be running.
/// It is only ever used when heatlog happens to be running as
/// administrator and the driver-safety check has cleared the installed
/// driver; otherwise this falls through to LibreHardwareMonitor and then
/// HWiNFO, exactly as before. A source named in the config is the only
/// one tried.
pub fn detect(cfg: &Config) -> Option<Box<dyn CpuTempSource>> {
    let pinned = cfg.temp_source.trim().to_ascii_lowercase();
    let wanted = |name: &str| pinned == "auto" || pinned.is_empty() || pinned == name;

    if wanted("pawnio") && driver_safety::status(cfg).usable {
        if let Ok(source) = pawnio::detect() {
            return Some(Box::new(source));
        }
    }
    if wanted("librehardwaremonitor") {
        let mut lhm = LhmWebServer {
            url: cfg.lhm_url.clone(),
        };
        if lhm.read().is_ok_and(|reading| reading.cpu_temp_c.is_some()) {
            return Some(Box::new(lhm));
        }
    }
    if wanted("hwinfo") && HwInfoShm::available() {
        return Some(Box::new(HwInfoShm));
    }
    None
}

/// Returns a source for fan speeds alone, for when the temperature source
/// reports no fans. Laptops are the common case: LibreHardwareMonitor
/// reads fans off a Super I/O chip that laptops do not have, while HWiNFO
/// asks the embedded controller the way the laptop maker's own app does.
/// `temp_label` is skipped, having already been asked.
pub fn detect_fans(lhm_url: &str, temp_label: Option<&str>) -> Option<Box<dyn CpuTempSource>> {
    if temp_label != Some("HWiNFO") && HwInfoShm.read_all().is_ok_and(|r| !r.fans.is_empty()) {
        return Some(Box::new(HwInfoShm));
    }
    if temp_label != Some("LibreHardwareMonitor") {
        let mut lhm = LhmWebServer {
            url: lhm_url.to_string(),
        };
        if lhm.read().is_ok_and(|r| !r.fans.is_empty()) {
            return Some(Box::new(lhm));
        }
    }
    None
}

/// What the setup page needs to tell the user where they stand.
pub struct Availability {
    pub source: Option<&'static str>,
    pub hwinfo_running: bool,
    pub hwinfo_sharing: bool,
    pub lhm_running: bool,
    pub lhm_web: bool,
    /// Where LibreHardwareMonitor is installed, if heatlog can find it.
    /// Its winget package is portable, so there is no Start menu entry to
    /// point the user at.
    pub lhm_path: Option<PathBuf>,
    /// Where HWiNFO is installed, so a fresh install is noticed without a
    /// restart.
    pub hwinfo_path: Option<PathBuf>,
    /// The installed PawnIO driver's version, when it is installed.
    pub pawnio_version: Option<String>,
    pub pawnio: PawnIo,
    /// A newer PawnIO than the installed one, when the check found one.
    pub pawnio_update: Option<String>,
}

/// What the setup page needs to say about PawnIO, in the order the user
/// can do something about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PawnIo {
    NotInstalled,
    /// Installed, but heatlog is not elevated, so the driver's
    /// administrators-only device refuses it.
    NeedsAdmin,
    /// Reachable, but heatlog will not use it, and why.
    Blocked(String),
    /// Cleared for use.
    Ready,
}

/// Where PawnIO stands right now: whether it is installed, whether this
/// heatlog can open it, and whether the safety check cleared it.
pub fn pawnio_state(cfg: &Config) -> PawnIo {
    if pawnio::installed_version().is_none() {
        return PawnIo::NotInstalled;
    }
    if let Err(e) = pawnio::probe() {
        return match e {
            OpenError::NotInstalled => PawnIo::NotInstalled,
            OpenError::NeedsAdmin => PawnIo::NeedsAdmin,
            other => PawnIo::Blocked(other.to_string()),
        };
    }
    let status = driver_safety::status(cfg);
    match status.reason {
        Some(reason) if !status.usable => PawnIo::Blocked(reason),
        _ if !status.usable => PawnIo::Blocked(
            "heatlog has not finished checking whether this driver is safe.".to_string(),
        ),
        _ => PawnIo::Ready,
    }
}

pub fn availability(cfg: &Config) -> Availability {
    let lhm_url = &cfg.lhm_url;
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let running = |needle: &str| {
        sys.processes().values().any(|process| {
            process
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains(needle)
        })
    };
    let hwinfo_sharing = HwInfoShm::available();
    let lhm_web = LhmWebServer {
        url: lhm_url.to_string(),
    }
    .read()
    .is_ok_and(|reading| reading.cpu_temp_c.is_some());
    Availability {
        source: if lhm_web {
            Some("LibreHardwareMonitor")
        } else if hwinfo_sharing {
            Some("HWiNFO")
        } else {
            None
        },
        hwinfo_running: running("hwinfo"),
        hwinfo_sharing,
        lhm_running: running("librehardwaremonitor"),
        lhm_web,
        lhm_path: find_lhm(&sys),
        hwinfo_path: find_hwinfo(&sys),
        pawnio_version: pawnio::installed_version(),
        pawnio: pawnio_state(cfg),
        pawnio_update: driver_safety::status(cfg).update_to,
    }
}

/// Looks for HWiNFO the same way: a running copy knows its own path, and
/// its installer puts it in Program Files under a name that depends on the
/// bitness.
fn find_hwinfo(sys: &System) -> Option<PathBuf> {
    if let Some(running) = running_exe(sys, "hwinfo") {
        return Some(running);
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        let Some(dir) = std::env::var_os(var) else {
            continue;
        };
        for folder in ["HWiNFO64", "HWiNFO32", "HWiNFO"] {
            for exe in ["HWiNFO64.exe", "HWiNFO32.exe"] {
                let path = PathBuf::from(&dir).join(folder).join(exe);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// The path of a running process whose name starts with `prefix`.
fn running_exe(sys: &System, prefix: &str) -> Option<PathBuf> {
    sys.processes()
        .values()
        .find(|process| {
            process
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with(prefix)
        })
        .and_then(|process| process.exe().map(Path::to_path_buf))
}

/// Looks for LibreHardwareMonitor: first by asking a running copy where it
/// came from, then in the places its installers put it (the winget package
/// is portable and lands in a versioned folder).
fn find_lhm(sys: &System) -> Option<PathBuf> {
    if let Some(running) = running_exe(sys, "librehardwaremonitor") {
        return Some(running);
    }

    let mut roots = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(
            PathBuf::from(&local)
                .join("Programs")
                .join("LibreHardwareMonitor"),
        );
        let packages = PathBuf::from(&local)
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        if let Ok(entries) = fs::read_dir(packages) {
            roots.extend(entries.flatten().map(|e| e.path()).filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("LibreHardwareMonitor"))
            }));
        }
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            roots.push(PathBuf::from(dir).join("LibreHardwareMonitor"));
        }
    }
    roots.iter().find_map(|root| find_exe(root, 2))
}

fn find_exe(dir: &Path, depth: usize) -> Option<PathBuf> {
    let direct = dir.join("LibreHardwareMonitor.exe");
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .find_map(|path| find_exe(&path, depth - 1))
}

struct LhmWebServer {
    url: String,
}

impl CpuTempSource for LhmWebServer {
    fn label(&self) -> &'static str {
        "LibreHardwareMonitor"
    }

    fn read(&mut self) -> Result<Reading> {
        let body = http_get(&self.url)?;
        let tree: Value = serde_json::from_str(&body).context("parsing LHM JSON")?;
        Ok(parse_lhm(&tree))
    }
}

/// Pulls the CPU temperature and every fan out of LHM's sensor tree.
pub fn parse_lhm(root: &Value) -> Reading {
    let mut best: Option<(bool, f32)> = None;
    let mut fans = Vec::new();
    walk(root, &mut Vec::new(), &mut best, &mut fans);
    Reading {
        cpu_temp_c: best.map(|(_, temp)| temp),
        cores: Vec::new(),
        fans,
    }
}

/// Walks LHM's sensor tree picking the best CPU temperature - a
/// package-level sensor ("CPU Package", AMD "Tctl") if present, otherwise
/// the hottest CPU-related sensor - and collecting the fans on the way.
fn walk(node: &Value, path: &mut Vec<String>, best: &mut Option<(bool, f32)>, fans: &mut Vec<Fan>) {
    let text = node.get("Text").and_then(Value::as_str).unwrap_or("");
    path.push(text.to_string());

    if let Some(celsius) = node
        .get("Value")
        .and_then(Value::as_str)
        .and_then(parse_celsius)
    {
        let cpu_related = path
            .iter()
            .any(|p| p.contains("CPU") || p.contains("Tctl") || p.contains("Tdie"));
        if cpu_related {
            let package = text.contains("Package") || text.contains("Tctl");
            let better = match best {
                None => true,
                Some((best_pkg, best_temp)) => {
                    (package && !*best_pkg) || (package == *best_pkg && celsius > *best_temp)
                }
            };
            if better {
                *best = Some((package, celsius));
            }
        }
    }

    if let Some(rpm) = node
        .get("Value")
        .and_then(Value::as_str)
        .and_then(parse_rpm)
    {
        fans.push(Fan {
            name: if text.is_empty() {
                "Fan".to_string()
            } else {
                text.to_string()
            },
            rpm,
        });
    }

    if let Some(children) = node.get("Children").and_then(Value::as_array) {
        for child in children {
            walk(child, path, best, fans);
        }
    }
    path.pop();
}

/// LHM writes fan speeds like "2400 RPM"; percentages of maximum are a
/// different sensor and are left alone.
fn parse_rpm(value: &str) -> Option<u32> {
    let number = value.strip_suffix("RPM")?.trim().replace(',', ".");
    let rpm = number.parse::<f32>().ok()?;
    (1.0..=20_000.0).contains(&rpm).then_some(rpm as u32)
}

/// LHM formats values like "67.5 °C", with a locale-dependent decimal
/// separator.
fn parse_celsius(value: &str) -> Option<f32> {
    let number = value.strip_suffix("°C")?.trim();
    number.replace(',', ".").parse().ok()
}

/// Minimal HTTP/1.0 GET, enough for LHM's plain-HTTP localhost endpoint.
/// 1.0 is deliberate: the server then sends an unchunked body and closes the
/// connection, so no HTTP client dependency is needed.
fn http_get(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("http://")
        .context("only http:// URLs are supported")?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };

    // connect_timeout, because a plain connect to a configured non-local
    // host that is down can block for the ~20s OS SYN timeout per sample.
    let addr = host
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}"))?
        .next()
        .with_context(|| format!("no address for {host}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .with_context(|| format!("connecting to {host}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (header, body) = response
        .split_once("\r\n\r\n")
        .context("malformed HTTP response")?;
    let status = header.lines().next().unwrap_or("");
    if !status.contains(" 200 ") {
        bail!("unexpected HTTP status: {status}");
    }
    Ok(body.to_string())
}
