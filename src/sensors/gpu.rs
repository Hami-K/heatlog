use anyhow::{Context, Result};
use nvml_wrapper::bitmasks::device::ThrottleReasons;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::Nvml;
use serde::{Deserialize, Serialize};

use super::wddm::{self, Vendor, WddmSensor};

/// Fixed facts about one GPU, recorded once per session.
#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    /// None when nothing trustworthy reports a total: an integrated GPU
    /// borrows system memory and has no fixed amount of its own.
    pub vram_total_mib: Option<u64>,
    /// Which interface the readings come from, for the user to see.
    pub source: &'static str,
    pub integrated: bool,
    /// Support heatlog has never been able to test on real hardware.
    pub experimental: bool,
}

/// One reading; also the on-disk sample format. Everything an interface may
/// not be able to tell us is optional, because a GPU that cannot report a
/// temperature must read as "not reported" and never as 0 °C.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GpuSample {
    pub util_pct: u32,
    pub vram_used_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_mem_used_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_util_pct: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f64>,
    /// None means this GPU cannot report throttling at all, which is not
    /// the same as reporting none. Reports must not confuse the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle: Option<Vec<String>>,
}

impl GpuSample {
    /// Graphics memory in use, whether it is the card's own or borrowed
    /// from the system.
    pub fn memory_used_mib(&self) -> u64 {
        self.vram_used_mib + self.shared_mem_used_mib.unwrap_or(0)
    }
}

/// One GPU, read through whichever interface covers it.
pub trait GpuSource {
    fn info(&self) -> &GpuInfo;
    fn read(&mut self) -> Result<GpuSample>;
}

/// Every GPU worth showing, the main one first: the dedicated GPU where
/// there is one, then the integrated GPU beside it.
///
/// An NVIDIA GPU is only ever read through NVML, which reports clocks,
/// power and throttle reasons that Windows' own interface does not. The
/// same adapter is therefore never read twice or listed twice.
pub fn detect() -> Vec<Box<dyn GpuSource>> {
    let nvml = NvmlSource::new().ok();
    let adapters = choose(nvml.is_some(), wddm::adapters().unwrap_or_default());

    let mut sources: Vec<Box<dyn GpuSource>> = Vec::new();
    if let Some(nvml) = nvml {
        sources.push(Box::new(nvml));
    }
    for adapter in adapters {
        if sources.len() == 2 {
            break;
        }
        if let Ok(source) = WddmSource::open(adapter) {
            sources.push(Box::new(source));
        }
    }
    sources
}

/// Which adapters to read through Windows, in order, given whether NVML
/// already covers one of them. Kept separate from the hardware so the
/// rules can be tested on a machine that has only one GPU.
fn choose(nvml_works: bool, adapters: Vec<wddm::Adapter>) -> Vec<wddm::Adapter> {
    let mut chosen: Vec<wddm::Adapter> = adapters
        .into_iter()
        // NVML reports clocks, power and throttle reasons that Windows
        // does not, so an NVIDIA GPU it has covered is never read here as
        // well: that would be the same GPU twice, with less in it.
        .filter(|adapter| !(nvml_works && adapter.vendor == Vendor::Nvidia))
        .collect();
    // One entry per adapter, whatever Windows listed.
    chosen.dedup_by_key(|adapter| adapter.luid);
    // The GPU a game runs on leads; the integrated one follows.
    chosen.sort_by_key(|adapter| adapter.integrated);
    chosen
}

/// Owns the NVML library handle. Reading a snapshot looks the device up each
/// time because `Device` borrows from `Nvml` and can't be stored alongside it.
pub struct NvmlSource {
    nvml: Nvml,
    info: GpuInfo,
}

impl NvmlSource {
    /// Fails cleanly when there is no NVIDIA driver/GPU (e.g. nvml.dll not
    /// present) — callers must treat that as "no NVIDIA GPU", not a crash.
    pub fn new() -> Result<Self> {
        let nvml = Nvml::init().context("NVML init failed - no NVIDIA driver/GPU found?")?;
        let device = nvml
            .device_by_index(0)
            .context("no NVIDIA GPU at index 0")?;
        let info = GpuInfo {
            name: device.name().context("reading GPU name")?,
            vram_total_mib: Some(
                device.memory_info().context("reading VRAM info")?.total / (1024 * 1024),
            ),
            source: "NVML",
            integrated: false,
            experimental: false,
        };
        Ok(Self { nvml, info })
    }
}

impl GpuSource for NvmlSource {
    fn info(&self) -> &GpuInfo {
        &self.info
    }

    fn read(&mut self) -> Result<GpuSample> {
        let device = self
            .nvml
            .device_by_index(0)
            .context("no NVIDIA GPU at index 0")?;

        let memory = device.memory_info().context("reading VRAM info")?;
        let util = device.utilization_rates().context("reading utilisation")?;

        Ok(GpuSample {
            temp_c: Some(
                device
                    .temperature(TemperatureSensor::Gpu)
                    .context("reading GPU temperature")?,
            ),
            core_mhz: Some(
                device
                    .clock_info(Clock::Graphics)
                    .context("reading core clock")?,
            ),
            mem_mhz: Some(
                device
                    .clock_info(Clock::Memory)
                    .context("reading memory clock")?,
            ),
            util_pct: util.gpu,
            mem_util_pct: Some(util.memory),
            power_w: Some(f64::from(device.power_usage().context("reading power draw")?) / 1000.0),
            vram_used_mib: memory.used / (1024 * 1024),
            shared_mem_used_mib: None,
            throttle: Some(throttle_reason_names(
                device
                    .current_throttle_reasons()
                    .context("reading throttle reasons")?,
            )),
        })
    }
}

/// An Intel or AMD GPU, read through Windows' own graphics interfaces.
pub struct WddmSource {
    sensor: WddmSensor,
    info: GpuInfo,
}

impl WddmSource {
    fn open(adapter: wddm::Adapter) -> Result<Self> {
        let sensor = WddmSensor::open(adapter.luid)?;
        let info = Self::label(&adapter);
        Ok(Self { sensor, info })
    }

    /// What to tell the user about this GPU before anything is read.
    fn label(adapter: &wddm::Adapter) -> GpuInfo {
        GpuInfo {
            name: adapter.name.clone(),
            // An integrated GPU has no memory of its own to measure
            // against; it borrows the system's.
            vram_total_mib: None,
            source: "Windows",
            integrated: adapter.integrated,
            // A dedicated AMD GPU is the one combination heatlog has never
            // run on, so it says so rather than implying it was tested.
            experimental: adapter.vendor == Vendor::Amd && !adapter.integrated,
        }
    }
}

impl GpuSource for WddmSource {
    fn info(&self) -> &GpuInfo {
        &self.info
    }

    fn read(&mut self) -> Result<GpuSample> {
        let reading = self.sensor.read()?;
        Ok(GpuSample {
            util_pct: reading.util_pct,
            vram_used_mib: reading.dedicated_used_mib,
            shared_mem_used_mib: Some(reading.shared_used_mib),
            temp_c: reading.temp_c,
            core_mhz: None,
            mem_mhz: reading.mem_mhz,
            mem_util_pct: None,
            power_w: None,
            // Windows exposes no throttle signal for these GPUs. Saying
            // "none" here would be a claim heatlog cannot make.
            throttle: None,
        })
    }
}

fn throttle_reason_names(reasons: ThrottleReasons) -> Vec<String> {
    const KNOWN: &[(ThrottleReasons, &str)] = &[
        (ThrottleReasons::SW_POWER_CAP, "power limit (software)"),
        (ThrottleReasons::HW_SLOWDOWN, "hardware slowdown"),
        (
            ThrottleReasons::HW_THERMAL_SLOWDOWN,
            "thermal limit (hardware)",
        ),
        (
            ThrottleReasons::SW_THERMAL_SLOWDOWN,
            "thermal limit (software)",
        ),
        (ThrottleReasons::HW_POWER_BRAKE_SLOWDOWN, "power brake"),
        (
            ThrottleReasons::APPLICATIONS_CLOCKS_SETTING,
            "application clock setting",
        ),
        (
            ThrottleReasons::DISPLAY_CLOCK_SETTING,
            "display clock setting",
        ),
        (ThrottleReasons::SYNC_BOOST, "sync boost"),
    ];
    // GPU_IDLE and NONE are deliberately not listed: idle clocks are normal,
    // not a throttle event worth reporting.
    KNOWN
        .iter()
        .filter(|(flag, _)| reasons.contains(*flag))
        .map(|(_, label)| (*label).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::wddm::{Adapter, Luid};

    fn adapter(name: &str, vendor: Vendor, integrated: bool, luid: u32) -> Adapter {
        Adapter {
            name: name.to_string(),
            luid: Luid { high: 0, low: luid },
            vendor,
            integrated,
            dedicated: !integrated,
        }
    }

    fn names(adapters: &[Adapter]) -> Vec<&str> {
        adapters.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn a_laptop_with_two_gpus_reads_each_through_the_right_interface() {
        let both = || {
            vec![
                adapter("Intel(R) Iris(R) Xe Graphics", Vendor::Intel, true, 1),
                adapter(
                    "NVIDIA GeForce RTX 4060 Laptop GPU",
                    Vendor::Nvidia,
                    false,
                    2,
                ),
            ]
        };
        // NVML has the dedicated GPU: Windows is only asked about the
        // integrated one, so the same GPU is never read or listed twice.
        assert_eq!(
            names(&choose(true, both())),
            ["Intel(R) Iris(R) Xe Graphics"]
        );

        // Without a working NVIDIA driver, Windows is better than nothing,
        // and the dedicated GPU still comes first.
        assert_eq!(
            names(&choose(false, both())),
            [
                "NVIDIA GeForce RTX 4060 Laptop GPU",
                "Intel(R) Iris(R) Xe Graphics"
            ]
        );
    }

    #[test]
    fn an_amd_pair_keeps_the_dedicated_gpu_first() {
        let adapters = vec![
            adapter("AMD Radeon(TM) Graphics", Vendor::Amd, true, 1),
            adapter("AMD Radeon RX 7600M XT", Vendor::Amd, false, 2),
        ];
        assert_eq!(
            names(&choose(false, adapters)),
            ["AMD Radeon RX 7600M XT", "AMD Radeon(TM) Graphics"]
        );
    }

    #[test]
    fn one_gpu_is_listed_once() {
        let adapters = vec![
            adapter("Intel(R) Iris(R) Xe Graphics", Vendor::Intel, true, 1),
            adapter("Intel(R) Iris(R) Xe Graphics", Vendor::Intel, true, 1),
        ];
        assert_eq!(choose(false, adapters).len(), 1);
    }

    #[test]
    fn dedicated_amd_support_says_it_is_untested() {
        let dedicated =
            WddmSource::label(&adapter("AMD Radeon RX 7600M XT", Vendor::Amd, false, 1));
        assert!(dedicated.experimental);
        // Integrated AMD is part of this release and tested as such.
        let integrated =
            WddmSource::label(&adapter("AMD Radeon(TM) Graphics", Vendor::Amd, true, 1));
        assert!(!integrated.experimental);
        assert!(integrated.integrated);
        // Intel integrated graphics are the case this was built against.
        let intel = WddmSource::label(&adapter("Intel(R) Iris(R) Xe", Vendor::Intel, true, 1));
        assert!(!intel.experimental);
        assert_eq!(intel.source, "Windows");
    }
}
