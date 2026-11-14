use anyhow::{Context, Result};
use nvml_wrapper::bitmasks::device::ThrottleReasons;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::Nvml;

/// Owns the NVML library handle. Reading a snapshot looks the device up each
/// time because `Device` borrows from `Nvml` and can't be stored alongside it.
pub struct GpuSensor {
    nvml: Nvml,
}

#[derive(Debug)]
pub struct GpuSnapshot {
    pub name: String,
    pub temp_c: u32,
    pub core_clock_mhz: u32,
    pub mem_clock_mhz: u32,
    pub gpu_util_pct: u32,
    pub mem_util_pct: u32,
    pub power_w: f64,
    pub vram_used_mib: u64,
    pub vram_total_mib: u64,
    pub throttle_reasons: Vec<String>,
}

impl GpuSensor {
    /// Fails cleanly when there is no NVIDIA driver/GPU (e.g. nvml.dll not
    /// present) — callers must treat that as "GPU unavailable", not a crash.
    pub fn new() -> Result<Self> {
        let nvml = Nvml::init().context("NVML init failed - no NVIDIA driver/GPU found?")?;
        Ok(Self { nvml })
    }

    pub fn read(&self) -> Result<GpuSnapshot> {
        let device = self
            .nvml
            .device_by_index(0)
            .context("no NVIDIA GPU at index 0")?;

        let memory = device.memory_info().context("reading VRAM info")?;
        let util = device.utilization_rates().context("reading utilisation")?;

        Ok(GpuSnapshot {
            name: device.name().context("reading GPU name")?,
            temp_c: device
                .temperature(TemperatureSensor::Gpu)
                .context("reading GPU temperature")?,
            core_clock_mhz: device
                .clock_info(Clock::Graphics)
                .context("reading core clock")?,
            mem_clock_mhz: device
                .clock_info(Clock::Memory)
                .context("reading memory clock")?,
            gpu_util_pct: util.gpu,
            mem_util_pct: util.memory,
            power_w: f64::from(device.power_usage().context("reading power draw")?) / 1000.0,
            vram_used_mib: memory.used / (1024 * 1024),
            vram_total_mib: memory.total / (1024 * 1024),
            throttle_reasons: throttle_reason_names(
                device
                    .current_throttle_reasons()
                    .context("reading throttle reasons")?,
            ),
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
