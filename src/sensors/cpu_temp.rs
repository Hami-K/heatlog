use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use wmi::{COMLibrary, WMIConnection};

/// Windows exposes CPU temperature inconsistently, so sources are probed in
/// order and the whole feature degrades to "unavailable" rather than failing.
pub trait CpuTempSource {
    fn label(&self) -> &'static str;
    fn read_c(&mut self) -> Result<f32>;
}

/// Returns the first source that produces a plausible reading right now:
/// LibreHardwareMonitor's web server if it is running, then the ACPI thermal
/// zone via WMI (often absent or access-denied without admin), else None.
pub fn detect(lhm_url: &str) -> Option<Box<dyn CpuTempSource>> {
    let mut lhm = LhmWebServer {
        url: lhm_url.to_string(),
    };
    if lhm.read_c().is_ok() {
        return Some(Box::new(lhm));
    }
    if let Ok(mut zone) = WmiThermalZone::new() {
        if zone.read_c().is_ok() {
            return Some(Box::new(zone));
        }
    }
    None
}

struct LhmWebServer {
    url: String,
}

impl CpuTempSource for LhmWebServer {
    fn label(&self) -> &'static str {
        "LibreHardwareMonitor"
    }

    fn read_c(&mut self) -> Result<f32> {
        let body = http_get(&self.url)?;
        let tree: Value = serde_json::from_str(&body).context("parsing LHM JSON")?;
        find_cpu_temp(&tree).context("no CPU temperature in LHM data")
    }
}

/// Walks LHM's sensor tree picking the best CPU temperature: a package-level
/// sensor ("CPU Package", AMD "Tctl") if present, otherwise the hottest
/// CPU-related sensor.
fn find_cpu_temp(root: &Value) -> Option<f32> {
    let mut best: Option<(bool, f32)> = None;
    walk(root, &mut Vec::new(), &mut best);
    best.map(|(_, temp)| temp)
}

fn walk(node: &Value, path: &mut Vec<String>, best: &mut Option<(bool, f32)>) {
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

    if let Some(children) = node.get("Children").and_then(Value::as_array) {
        for child in children {
            walk(child, path, best);
        }
    }
    path.pop();
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

#[derive(Deserialize)]
#[serde(rename = "MSAcpi_ThermalZoneTemperature")]
#[serde(rename_all = "PascalCase")]
struct ThermalZone {
    current_temperature: u32,
}

struct WmiThermalZone {
    wmi: WMIConnection,
}

impl WmiThermalZone {
    fn new() -> Result<Self> {
        let com = COMLibrary::new().context("initialising COM")?;
        let wmi = WMIConnection::with_namespace_path("root\\WMI", com)
            .context("connecting to root\\WMI")?;
        Ok(Self { wmi })
    }
}

impl CpuTempSource for WmiThermalZone {
    fn label(&self) -> &'static str {
        "ACPI thermal zone"
    }

    fn read_c(&mut self) -> Result<f32> {
        let zones: Vec<ThermalZone> = self
            .wmi
            .query()
            .context("querying MSAcpi_ThermalZoneTemperature")?;
        // Thermal zones cover the whole board; the hottest is the closest
        // stand-in for the CPU. Values are tenths of a kelvin.
        let hottest = zones
            .iter()
            .map(|z| z.current_temperature)
            .max()
            .context("no thermal zones reported")?;
        let celsius = hottest as f32 / 10.0 - 273.15;
        ensure!(
            (0.0..=120.0).contains(&celsius),
            "implausible reading: {celsius} C"
        );
        Ok(celsius)
    }
}
