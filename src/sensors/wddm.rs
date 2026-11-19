//! Windows' own graphics stack as a sensor source, for the GPUs NVIDIA's
//! library cannot see: Intel and AMD, integrated or dedicated.
//!
//! Two unprivileged sources are combined. The adapters themselves come from
//! the kernel-mode thunk interface (`D3DKMT*`), which gives each one its
//! name, its LUID and whether it is the integrated or the dedicated half of
//! a laptop's pair. Utilisation and memory come from the same performance
//! counters Task Manager's GPU tab reads, matched to an adapter by that
//! LUID. No driver, no admin rights.

use std::collections::HashMap;
use std::mem::size_of;

use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use windows::Wdk::Graphics::Direct3D::{
    D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo,
    D3DKMT_ADAPTERINFO, D3DKMT_ADAPTERREGISTRYINFO, D3DKMT_ADAPTERTYPE, D3DKMT_ADAPTER_PERFDATA,
    D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2, D3DKMT_OPENADAPTERFROMLUID, D3DKMT_QUERYADAPTERINFO,
    KMTQAITYPE_ADAPTERPERFDATA, KMTQAITYPE_ADAPTERREGISTRYINFO, KMTQAITYPE_ADAPTERTYPE,
    KMTQUERYADAPTERINFOTYPE,
};
use wmi::{COMLibrary, WMIConnection};

/// A locally unique id for an adapter: how both the kernel interface and
/// the performance counters name the same GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Luid {
    pub high: i32,
    pub low: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Intel,
    Amd,
    Other,
}

/// What `D3DKMT` can say about one graphics adapter before anything is read
/// from it.
#[derive(Debug, Clone)]
pub struct Adapter {
    pub name: String,
    pub luid: Luid,
    pub vendor: Vendor,
    /// The integrated half of a laptop's pair, as the driver reports it.
    pub integrated: bool,
    /// The dedicated half, as the driver reports it. Both false on a
    /// machine with a single GPU, which says nothing either way.
    pub dedicated: bool,
}

/// Every real graphics adapter Windows knows about. Software adapters (the
/// Microsoft Basic Render Driver) are left out: they are not hardware and
/// have nothing to report.
pub fn adapters() -> Result<Vec<Adapter>> {
    let mut found = Vec::new();
    // Unavoidable unsafe: the thunk interface has no safe wrapper. Each
    // call writes into one local struct, and every adapter handle the
    // enumeration opens is closed again below.
    unsafe {
        let mut query = D3DKMT_ENUMADAPTERS2::default();
        D3DKMTEnumAdapters2(&mut query)
            .ok()
            .context("asking Windows for its graphics adapters")?;
        let mut list = vec![D3DKMT_ADAPTERINFO::default(); query.NumAdapters as usize];
        query.pAdapters = list.as_mut_ptr();
        let status = D3DKMTEnumAdapters2(&mut query);
        if status.is_err() {
            for info in &list {
                close(info.hAdapter);
            }
            status.ok().context("listing graphics adapters")?;
        }

        for info in list.iter().take(query.NumAdapters as usize) {
            let kind = adapter_type(info.hAdapter);
            let name = adapter_name(info.hAdapter);
            close(info.hAdapter);
            let Some((software, integrated, dedicated)) = kind else {
                continue;
            };
            if software {
                continue;
            }
            let Some(name) = name else { continue };
            found.push(Adapter {
                vendor: vendor_of(&name),
                name,
                luid: Luid {
                    high: info.AdapterLuid.HighPart,
                    low: info.AdapterLuid.LowPart,
                },
                integrated,
                dedicated,
            });
        }
    }
    found.dedup_by_key(|adapter| adapter.luid);
    ensure!(!found.is_empty(), "Windows reported no graphics adapter");
    Ok(found)
}

/// The vendor, from the name the driver registered. Only used to decide
/// which GPU a dedicated library already covers, and which support is still
/// experimental.
fn vendor_of(name: &str) -> Vendor {
    let name = name.to_ascii_lowercase();
    if name.contains("nvidia") || name.contains("geforce") {
        Vendor::Nvidia
    } else if name.contains("intel") {
        Vendor::Intel
    } else if name.contains("amd") || name.contains("radeon") {
        Vendor::Amd
    } else {
        Vendor::Other
    }
}

/// Returns (software, integrated, dedicated) from the adapter's type
/// bitfield. The bits are fixed by the Windows headers.
unsafe fn adapter_type(handle: u32) -> Option<(bool, bool, bool)> {
    const SOFTWARE_DEVICE: u32 = 1 << 2;
    const HYBRID_DISCRETE: u32 = 1 << 4;
    const HYBRID_INTEGRATED: u32 = 1 << 5;
    let kind: D3DKMT_ADAPTERTYPE = query(handle, KMTQAITYPE_ADAPTERTYPE).ok()?;
    // The struct is a union of named bits and the same 32 bits as a value.
    let bits = kind.Anonymous.Value;
    Some((
        bits & SOFTWARE_DEVICE != 0,
        bits & HYBRID_INTEGRATED != 0,
        bits & HYBRID_DISCRETE != 0,
    ))
}

unsafe fn adapter_name(handle: u32) -> Option<String> {
    let info: D3DKMT_ADAPTERREGISTRYINFO = query(handle, KMTQAITYPE_ADAPTERREGISTRYINFO).ok()?;
    let name = wide_to_string(&info.AdapterString);
    (!name.is_empty()).then_some(name)
}

fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end]).trim().to_string()
}

unsafe fn query<T: Default>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE) -> Result<T> {
    query_with(handle, kind, T::default())
}

/// Some queries take input in the same struct they fill in, so the caller
/// hands over a prepared value.
unsafe fn query_with<T>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE, mut data: T) -> Result<T> {
    let mut request = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: kind,
        pPrivateDriverData: (&mut data as *mut T).cast(),
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    D3DKMTQueryAdapterInfo(&mut request)
        .ok()
        .context("querying a graphics adapter")?;
    Ok(data)
}

unsafe fn close(handle: u32) {
    if handle != 0 {
        let _ = D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: handle });
    }
}

/// What the driver will tell us about load and heat, all of it optional:
/// integrated GPUs commonly report no temperature at all, and a zero there
/// must never be shown as 0 °C.
#[derive(Debug, Default, PartialEq)]
pub struct WddmReading {
    pub util_pct: u32,
    pub dedicated_used_mib: u64,
    pub shared_used_mib: u64,
    pub temp_c: Option<u32>,
    pub mem_mhz: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename = "Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine")]
#[serde(rename_all = "PascalCase")]
struct EngineRow {
    name: String,
    utilization_percentage: u64,
}

#[derive(Deserialize)]
#[serde(rename = "Win32_PerfFormattedData_GPUPerformanceCounters_GPUAdapterMemory")]
#[serde(rename_all = "PascalCase")]
struct MemoryRow {
    name: String,
    dedicated_usage: u64,
    shared_usage: u64,
}

pub struct WddmSensor {
    wmi: WMIConnection,
    luid: Luid,
}

impl WddmSensor {
    pub fn open(luid: Luid) -> Result<Self> {
        let com = COMLibrary::new().context("initialising COM")?;
        let wmi = WMIConnection::new(com).context("connecting to WMI")?;
        Ok(Self { wmi, luid })
    }

    pub fn read(&self) -> Result<WddmReading> {
        let engines: Vec<EngineRow> = self.wmi.query().context("querying GPU engine counters")?;
        let memory: Vec<MemoryRow> = self.wmi.query().context("querying GPU memory counters")?;

        let (dedicated_used_mib, shared_used_mib) = memory
            .iter()
            .find(|row| luid_of(&row.name) == Some(self.luid))
            .map(|row| {
                (
                    row.dedicated_usage / (1024 * 1024),
                    row.shared_usage / (1024 * 1024),
                )
            })
            .unwrap_or((0, 0));

        let perf = self.perf_data();
        Ok(WddmReading {
            util_pct: utilisation(&engines, self.luid),
            dedicated_used_mib,
            shared_used_mib,
            temp_c: perf.and_then(|p| plausible_temp(p.Temperature)),
            mem_mhz: perf.and_then(|p| plausible_mhz(p.MemoryFrequency)),
        })
    }

    /// Temperature and memory clock, when the driver fills them in. Many
    /// integrated GPUs leave them at zero, which is why every field is
    /// checked for plausibility rather than trusted.
    fn perf_data(&self) -> Option<D3DKMT_ADAPTER_PERFDATA> {
        // Unavoidable unsafe, as above: open by id, one query, close.
        unsafe {
            let mut open = D3DKMT_OPENADAPTERFROMLUID {
                AdapterLuid: windows::Win32::Foundation::LUID {
                    LowPart: self.luid.low,
                    HighPart: self.luid.high,
                },
                hAdapter: 0,
            };
            D3DKMTOpenAdapterFromLuid(&mut open).ok().ok()?;
            let data = query_with(
                open.hAdapter,
                KMTQAITYPE_ADAPTERPERFDATA,
                D3DKMT_ADAPTER_PERFDATA {
                    PhysicalAdapterIndex: 0,
                    ..Default::default()
                },
            );
            close(open.hAdapter);
            data.ok()
        }
    }
}

/// The driver reports tenths of a degree. Anything outside a range a
/// working GPU could be in means "not reported".
fn plausible_temp(deci_celsius: u32) -> Option<u32> {
    let celsius = deci_celsius / 10;
    (10..=120).contains(&celsius).then_some(celsius)
}

fn plausible_mhz(hertz: u64) -> Option<u32> {
    let mhz = hertz / 1_000_000;
    (1..=20_000).contains(&mhz).then_some(mhz as u32)
}

/// Task Manager shows the busiest engine, not the sum of all of them, so a
/// GPU running video decode and 3D at half load each reads 50%, not 100%.
/// Instances are per process, so each engine type is totalled first.
fn utilisation(rows: &[EngineRow], luid: Luid) -> u32 {
    let mut per_engine: HashMap<&str, u64> = HashMap::new();
    for row in rows {
        if luid_of(&row.name) != Some(luid) {
            continue;
        }
        if let Some(engine) = engine_type(&row.name) {
            *per_engine.entry(engine).or_default() += row.utilization_percentage;
        }
    }
    per_engine
        .into_values()
        .max()
        .unwrap_or(0)
        .min(100)
        .try_into()
        .unwrap_or(0)
}

/// Counter instances are named like
/// "pid_1234_luid_0x00000000_0x0000D1B4_phys_0_eng_0_engtype_3D".
fn luid_of(instance: &str) -> Option<Luid> {
    let rest = instance.split("luid_").nth(1)?;
    let mut parts = rest.split('_');
    let first = hex(parts.next()?)?;
    let second = hex(parts.next()?)?;
    Some(Luid {
        high: first as i32,
        low: second,
    })
}

fn hex(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn engine_type(instance: &str) -> Option<&str> {
    instance.split("engtype_").nth(1).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(instance: &str, pct: u64) -> EngineRow {
        EngineRow {
            name: instance.to_string(),
            utilization_percentage: pct,
        }
    }

    #[test]
    fn utilisation_is_the_busiest_engine_of_the_right_adapter() {
        let ours = Luid {
            high: 0,
            low: 0x0001_4F2D,
        };
        let rows = vec![
            // Two processes using the 3D engine: those add up.
            engine(
                "pid_1_luid_0x00000000_0x00014F2D_phys_0_eng_0_engtype_3D",
                20,
            ),
            engine(
                "pid_2_luid_0x00000000_0x00014F2D_phys_0_eng_0_engtype_3D",
                15,
            ),
            // A different engine on the same GPU: not added to 3D.
            engine(
                "pid_2_luid_0x00000000_0x00014F2D_phys_0_eng_4_engtype_VideoDecode",
                80,
            ),
            // The other GPU in the machine, which must not leak in.
            engine(
                "pid_3_luid_0x00000000_0x00015612_phys_0_eng_0_engtype_3D",
                99,
            ),
        ];
        assert_eq!(utilisation(&rows, ours), 80);

        // Nothing for an adapter with no instances at all.
        let idle = Luid { high: 0, low: 1 };
        assert_eq!(utilisation(&rows, idle), 0);
    }

    #[test]
    fn a_zero_reading_is_not_reported_as_zero_degrees() {
        assert_eq!(plausible_temp(0), None);
        assert_eq!(plausible_temp(615), Some(61));
        assert_eq!(plausible_temp(99_999), None);
        assert_eq!(plausible_mhz(0), None);
        assert_eq!(plausible_mhz(1_750_000_000), Some(1750));
    }

    #[test]
    fn instance_names_are_read_the_way_windows_writes_them() {
        assert_eq!(
            luid_of("pid_9_luid_0x00000000_0x0000D1B4_phys_0_eng_0_engtype_3D"),
            Some(Luid {
                high: 0,
                low: 0xD1B4
            })
        );
        assert_eq!(luid_of("something else entirely"), None);
        assert_eq!(
            engine_type("pid_9_luid_0x0_0x1_phys_0_eng_3_engtype_VideoProcessing"),
            Some("VideoProcessing")
        );
        // Unnamed engines exist; they are skipped rather than lumped
        // together under an empty name.
        assert_eq!(
            engine_type("pid_9_luid_0x0_0x1_phys_0_eng_11_engtype_"),
            None
        );
    }
}
