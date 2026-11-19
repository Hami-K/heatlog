//! CPU temperature read from the CPU itself, through the PawnIO driver.
//!
//! PawnIO is a signed kernel driver that runs small, separately signed
//! modules. heatlog loads the official Intel and AMD MSR modules - never
//! its own, never rewritten - and reads the registers the CPU reports its
//! own temperature in. It only ever reads.
//!
//! The driver's device is open to administrators alone: PawnIO's own INF
//! sets `D:P(A;;GA;;;SY)(A;;GA;;;BA)` on it, so an ordinary user's
//! `pawnio_open` returns E_ACCESSDENIED. heatlog therefore never asks for
//! elevation: this source exists when the user chose to start heatlog as
//! administrator, and every other run falls back to LibreHardwareMonitor
//! or HWiNFO exactly as before.

use std::ffi::{c_char, CStr, OsStr};
use std::fs;
use std::io::Read as _;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

use super::cpu_temp::{CpuTempSource, Reading};

/// The uninstall entry PawnIO's installer writes, which is also how
/// LibreHardwareMonitor finds out whether PawnIO is installed.
const UNINSTALL_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO";

/// The official signed module bundle, pinned to one release and checked
/// against its hash. heatlog never ships PawnIO binaries itself: the
/// modules are LGPL and come from their author, and the driver verifies
/// their signature before it runs them.
const MODULES_TAG: &str = "0.2.12";
const MODULES_URL: &str =
    "https://github.com/namazso/PawnIO.Modules/releases/download/0.2.12/release_0_2_12.zip";
const MODULES_SHA256: &str = "09698897ce19b92f1043a2e322cfcf0b1fe823e0b576971b178a65244fb0af9d";
const INTEL_MODULE: &str = "IntelMSR.bin";
const AMD_MODULE: &str = "AMDFamily17.bin";

/// Intel: IA32_THERM_STATUS is per core, IA32_PACKAGE_THERM_STATUS is the
/// package, and both report how far *below* Tjmax the CPU is.
const MSR_THERM_STATUS: u64 = 0x19C;
const MSR_TEMPERATURE_TARGET: u64 = 0x1A2;
const MSR_PACKAGE_THERM_STATUS: u64 = 0x1B1;
/// AMD family 17h and later: Tctl sits in the SMN, not an MSR.
const SMN_CUR_TEMP: u64 = 0x0005_9800;

/// Why PawnIO is not being used. The GUI turns each of these into one
/// line, because "it did not work" is not something a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// PawnIO is not installed, or its library is missing.
    NotInstalled,
    /// Installed, but heatlog is not running as administrator.
    NeedsAdmin,
    /// Installed and heatlog is elevated, but Windows would not give us
    /// the device. The usual cause is that the driver never loaded, which
    /// is what Microsoft's vulnerable-driver blocklist does to a driver.
    DriverRefused,
    /// Everything opened, but this CPU is not one the official modules
    /// can read. Nothing is guessed in that case.
    Unsupported(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "PawnIO is not installed"),
            Self::NeedsAdmin => write!(f, "run heatlog as administrator to use PawnIO"),
            Self::DriverRefused => write!(f, "Windows would not load the PawnIO driver"),
            Self::Unsupported(why) => write!(f, "{why}"),
        }
    }
}

/// The installed PawnIO version, from the same registry value PawnIO's
/// other consumers read. None when it is not installed.
pub fn installed_version() -> Option<String> {
    crate::update::reg_value(UNINSTALL_KEY, "DisplayVersion")
}

/// Opens the driver and closes it again, just to find out where we
/// stand. No module is loaded, so this is cheap enough for the setup page
/// to ask every few seconds.
pub fn probe() -> Result<(), OpenError> {
    Executor::open(Lib::get()?).map(|_| ())
}

type OpenFn = unsafe extern "system" fn(*mut isize) -> i32;
type LoadFn = unsafe extern "system" fn(isize, *const u8, usize) -> i32;
type ExecuteFn = unsafe extern "system" fn(
    isize,
    *const c_char,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> i32;
type CloseFn = unsafe extern "system" fn(isize) -> i32;

/// PawnIOLib.dll, loaded from the install at run time. heatlog neither
/// bundles nor links against it, which is what keeps heatlog MIT beside
/// PawnIO's own licence.
#[derive(Clone, Copy)]
struct Lib {
    open: OpenFn,
    load: LoadFn,
    execute: ExecuteFn,
    close: CloseFn,
}

static LIB: std::sync::OnceLock<Lib> = std::sync::OnceLock::new();

impl Lib {
    /// The library, loaded at most once per run. Only success is
    /// remembered: someone who installs PawnIO while heatlog is open
    /// should not have to restart it.
    fn get() -> Result<Self, OpenError> {
        if let Some(lib) = LIB.get() {
            return Ok(*lib);
        }
        let lib = Self::load()?;
        let _ = LIB.set(lib);
        Ok(lib)
    }

    fn load() -> Result<Self, OpenError> {
        let dll = library_path().ok_or(OpenError::NotInstalled)?;
        let wide: Vec<u16> = OsStr::new(&dll)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: a NUL-terminated path. The module is never freed, so
        // the function pointers below stay valid for the whole process.
        let module =
            unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }.map_err(|_| OpenError::NotInstalled)?;
        // SAFETY: each name is a documented export of PawnIOLib with the
        // signature of the matching type above.
        unsafe {
            Ok(Self {
                open: symbol(module, c"pawnio_open")?,
                load: symbol(module, c"pawnio_load")?,
                execute: symbol(module, c"pawnio_execute")?,
                close: symbol(module, c"pawnio_close")?,
            })
        }
    }
}

/// SAFETY: the caller must name an export that really has type `F`, which
/// is a bare function pointer in every use here.
unsafe fn symbol<F>(module: HMODULE, name: &CStr) -> Result<F, OpenError> {
    let address =
        GetProcAddress(module, PCSTR(name.as_ptr().cast())).ok_or(OpenError::NotInstalled)?;
    Ok(std::mem::transmute_copy(&address))
}

/// Where PawnIOLib.dll is: the installer records its folder, and Program
/// Files is the fallback for an install that predates that.
fn library_path() -> Option<String> {
    let dll = |dir: PathBuf| {
        let path = dir.join("PawnIOLib.dll");
        path.is_file().then(|| path.to_string_lossy().into_owned())
    };
    if let Some(dir) = crate::update::reg_value(UNINSTALL_KEY, "InstallLocation") {
        if let Some(path) = dll(PathBuf::from(dir)) {
            return Some(path);
        }
    }
    std::env::var_os("ProgramFiles")
        .map(|dir| PathBuf::from(dir).join("PawnIO"))
        .and_then(dll)
}

/// One open executor with one module loaded into it.
struct Executor {
    lib: Lib,
    handle: isize,
}

impl Executor {
    fn open(lib: Lib) -> Result<Self, OpenError> {
        let mut handle = 0isize;
        // SAFETY: a valid out pointer; the HRESULT says whether it was
        // written.
        let hr = unsafe { (lib.open)(&mut handle) } as u32;
        match hr {
            0 => Ok(Self { lib, handle }),
            // E_ACCESSDENIED: the device's ACL allows administrators only.
            0x8007_0005 => Err(OpenError::NeedsAdmin),
            _ => Err(OpenError::DriverRefused),
        }
    }

    fn load(&self, blob: &[u8]) -> bool {
        // SAFETY: a handle from pawnio_open, and a slice we own.
        unsafe { (self.lib.load)(self.handle, blob.as_ptr(), blob.len()) == 0 }
    }

    /// Runs one module function. `out` is filled in place; an error means
    /// the register could not be read, never a made-up value.
    fn execute(&self, name: &CStr, input: &[u64], out: &mut [u64]) -> Result<()> {
        let mut written = 0usize;
        // SAFETY: each buffer is passed with its own length, and the name
        // is NUL-terminated.
        let hr = unsafe {
            (self.lib.execute)(
                self.handle,
                name.as_ptr(),
                input.as_ptr(),
                input.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut written,
            )
        };
        if hr != 0 {
            bail!("PawnIO {} failed (0x{:08x})", name.to_string_lossy(), hr);
        }
        if written < out.len() {
            bail!("PawnIO returned {written} values, expected {}", out.len());
        }
        Ok(())
    }

    fn read_msr(&self, msr: u64) -> Result<u64> {
        let mut out = [0u64; 1];
        self.execute(c"ioctl_read_msr", &[msr], &mut out)?;
        Ok(out[0])
    }

    fn read_smn(&self, address: u64) -> Result<u64> {
        let mut out = [0u64; 1];
        self.execute(c"ioctl_read_smn", &[address], &mut out)?;
        Ok(out[0])
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        unsafe { (self.lib.close)(self.handle) };
    }
}

enum Kind {
    Intel { tjmax: u32, cores: u32 },
    Amd,
}

/// How often the per-core registers are read, in samples. Reading them
/// means an affinity switch and a driver call per logical processor,
/// which measured 2.1% of one core at one sample a second on a 20-thread
/// laptop - most of the recorder's whole cost, for a figure that only has
/// to look current. The package reading, the one that gets recorded, is
/// still taken every sample.
const CORES_EVERY: u32 = 10;

/// CPU temperature straight from the CPU's own registers.
pub struct PawnIoTemp {
    executor: Executor,
    kind: Kind,
    /// The last per-core readings, refreshed every `CORES_EVERY` samples.
    cores: Vec<f32>,
    samples: u32,
}

/// Opens PawnIO and loads whichever official module fits this CPU. Each
/// module refuses the wrong vendor itself, so there is no CPU detection
/// here to get wrong.
pub fn detect() -> Result<PawnIoTemp, OpenError> {
    let intel = Executor::open(Lib::get()?)?;
    if cached_module(INTEL_MODULE).is_ok_and(|blob| intel.load(&blob)) {
        let tjmax = intel
            .read_msr(MSR_TEMPERATURE_TARGET)
            .ok()
            .and_then(tjmax_of)
            .ok_or_else(|| {
                OpenError::Unsupported(
                    "this Intel CPU does not report the temperature limit PawnIO needs".into(),
                )
            })?;
        return Ok(PawnIoTemp {
            executor: intel,
            kind: Kind::Intel {
                tjmax,
                cores: logical_cores(),
            },
            cores: Vec::new(),
            samples: 0,
        });
    }
    drop(intel);

    let amd = Executor::open(Lib::get()?)?;
    if cached_module(AMD_MODULE).is_ok_and(|blob| amd.load(&blob))
        && amd.read_smn(SMN_CUR_TEMP).ok().and_then(tctl_of).is_some()
    {
        return Ok(PawnIoTemp {
            executor: amd,
            kind: Kind::Amd,
            cores: Vec::new(),
            samples: 0,
        });
    }
    Err(OpenError::Unsupported(
        "PawnIO's official modules do not cover this CPU".into(),
    ))
}

/// Affinity masks hold 64 bits, so that is as far as the per-core read
/// goes; the package reading does not depend on it.
fn logical_cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get().min(64) as u32)
        .unwrap_or(1)
}

impl CpuTempSource for PawnIoTemp {
    fn label(&self) -> &'static str {
        "PawnIO"
    }

    /// Temperature only: the official modules have nothing for a laptop's
    /// fans, so fan speeds still come from HWiNFO or
    /// LibreHardwareMonitor.
    fn read(&mut self) -> Result<Reading> {
        let cpu_temp_c = match self.kind {
            Kind::Intel { tjmax, cores } => {
                let package = self
                    .executor
                    .read_msr(MSR_PACKAGE_THERM_STATUS)
                    .ok()
                    .and_then(|status| intel_temp(status, tjmax));
                // The cores cost a driver call each, so they are read
                // now and then - or every time when they are the only
                // reading this CPU has.
                if package.is_none() || self.samples.is_multiple_of(CORES_EVERY) {
                    self.cores = self.intel_cores(tjmax, cores);
                }
                self.samples = self.samples.wrapping_add(1);
                // Not every Intel CPU has the package register. The
                // hottest core is then the honest stand-in; it is a
                // reading, not a guess.
                package.or_else(|| hottest(&self.cores))
            }
            Kind::Amd => tctl_of(self.executor.read_smn(SMN_CUR_TEMP)?),
        };
        if cpu_temp_c.is_none() {
            bail!("PawnIO read no usable temperature");
        }
        Ok(Reading {
            cpu_temp_c,
            cores: self.cores.clone(),
            fans: Vec::new(),
        })
    }
}

fn hottest(temps: &[f32]) -> Option<f32> {
    temps.iter().copied().reduce(f32::max)
}

impl PawnIoTemp {
    /// Reads IA32_THERM_STATUS on each logical processor in turn. That
    /// MSR is per core, so it has to be read *on* the core in question,
    /// which is what the affinity dance is for.
    fn intel_cores(&self, tjmax: u32, cores: u32) -> Vec<f32> {
        let mut temps = Vec::new();
        for core in 0..cores {
            // SAFETY: a mask with one bit inside this process's group.
            let previous = unsafe { SetThreadAffinityMask(GetCurrentThread(), 1usize << core) };
            if previous == 0 {
                break;
            }
            let temp = self
                .executor
                .read_msr(MSR_THERM_STATUS)
                .ok()
                .and_then(|status| intel_temp(status, tjmax));
            // SAFETY: restoring the mask this thread came with.
            unsafe { SetThreadAffinityMask(GetCurrentThread(), previous) };
            if let Some(temp) = temp {
                if !temps.contains(&temp) {
                    temps.push(temp);
                }
            }
        }
        temps.sort_by(f32::total_cmp);
        temps
    }
}

/// Tjmax, the temperature the CPU counts down from, out of
/// IA32_TEMPERATURE_TARGET bits 23:16. Anything outside the range real
/// CPUs use means the register is not what we think it is.
pub fn tjmax_of(target: u64) -> Option<u32> {
    let tjmax = ((target >> 16) & 0xFF) as u32;
    (60..=130).contains(&tjmax).then_some(tjmax)
}

/// A THERM_STATUS register as degrees. Bit 31 means "the digital readout
/// below is valid"; bits 22:16 are how many degrees below Tjmax the CPU
/// is. Without the valid bit there is no reading - not a zero.
pub fn intel_temp(status: u64, tjmax: u32) -> Option<f32> {
    if status & (1 << 31) == 0 {
        return None;
    }
    let delta = ((status >> 16) & 0x7F) as u32;
    let temp = tjmax.checked_sub(delta)?;
    (1..=130).contains(&temp).then_some(temp as f32)
}

/// AMD family 17h and later report Tctl in SMN register THM_TCON_CUR_TMP:
/// bits 31:21 in steps of 0.125 °C, with a 49 °C offset applied when the
/// range-select bit (19) or both TJ_SEL bits (17:16) are set.
pub fn tctl_of(raw: u64) -> Option<f32> {
    let raw = (raw & 0xFFFF_FFFF) as u32;
    let offset = raw & 0x8_0000 != 0 || raw & 0x3_0000 == 0x3_0000;
    let mut temp = ((raw >> 21) as f32) * 0.125;
    if offset {
        temp -= 49.0;
    }
    (1.0..=130.0).contains(&temp).then_some(temp)
}

fn modules_dir() -> Result<PathBuf> {
    Ok(crate::config::data_dir()?
        .join("pawnio-modules")
        .join(MODULES_TAG))
}

/// One module out of the cache. Reading only: nothing on the way to a
/// temperature reading is allowed to wait on the network.
fn cached_module(name: &str) -> Result<Vec<u8>> {
    let path = modules_dir()?.join(name);
    fs::read(&path).with_context(|| format!("reading {}", path.display()))
}

/// Downloads the official module bundle once, if it is not cached
/// already. Called from the background driver-safety check, never from
/// the sampling path - an unreachable GitHub must not hold up the first
/// reading. An unverified blob is never written, let alone run.
pub fn ensure_modules() -> Result<()> {
    let dir = modules_dir()?;
    if [INTEL_MODULE, AMD_MODULE]
        .iter()
        .all(|name| dir.join(name).is_file())
    {
        return Ok(());
    }
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let mut zip = Vec::new();
    ureq::get(MODULES_URL)
        .set("User-Agent", "heatlog")
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .context("downloading PawnIO's official modules")?
        .into_reader()
        .read_to_end(&mut zip)
        .context("reading PawnIO's official modules")?;
    if format!("{:x}", Sha256::digest(&zip)) != MODULES_SHA256 {
        bail!("the PawnIO module bundle did not match its published checksum");
    }
    let archive = dir.join("modules.zip");
    fs::write(&archive, &zip).with_context(|| format!("writing {}", archive.display()))?;
    extract(&archive, &dir)?;
    let _ = fs::remove_file(&archive);
    Ok(())
}

/// Windows has shipped bsdtar in System32 since Windows 10 1803 and it
/// reads zip files. A missing or unhappy tar simply means no PawnIO.
fn extract(archive: &Path, dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
        .join("tar.exe");
    let status = std::process::Command::new(tar)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .args([INTEL_MODULE, AMD_MODULE])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| anyhow!("unpacking PawnIO's modules: {e}"))?;
    if !status.success() {
        bail!("could not unpack PawnIO's official modules");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_intel_register_pair_decodes_to_the_temperature_the_cpu_reports() {
        // Tjmax 100 °C, as an i9-13900H reports it.
        assert_eq!(tjmax_of(0x0064_0000), Some(100));
        // Valid bit set, readout 0x34 = 52 degrees below the limit.
        assert_eq!(intel_temp(0x8834_0000, 100), Some(48.0));
        // Right at the limit.
        assert_eq!(intel_temp(0x8800_0000, 100), Some(100.0));
    }

    #[test]
    fn a_reading_the_cpu_calls_invalid_is_never_shown() {
        // Bit 31 clear: the CPU is saying the readout means nothing.
        assert_eq!(intel_temp(0x0034_0000, 100), None);
        // A Tjmax no CPU has: the register is not what we think it is.
        assert_eq!(tjmax_of(0x000A_0000), None);
        assert_eq!(tjmax_of(0), None);
        // A readout deeper than Tjmax would wrap round to a huge number.
        assert_eq!(intel_temp(0x8864_0000, 100), None);
    }

    #[test]
    fn amd_tctl_uses_the_offset_the_register_asks_for() {
        // 0x3C000000 >> 21 = 480 steps of 0.125 °C = 60 °C.
        assert_eq!(tctl_of(0x3C00_0000), Some(60.0));
        // The same reading with range-select set is 49 °C lower.
        assert_eq!(tctl_of(0x3C08_0000), Some(11.0));
        // TJ_SEL = 3 means the same offset.
        assert_eq!(tctl_of(0x3C03_0000), Some(11.0));
        // An empty register is not 0 °C, it is no reading at all.
        assert_eq!(tctl_of(0), None);
    }

    #[test]
    fn the_hottest_core_stands_in_for_a_missing_package_reading() {
        assert_eq!(hottest(&[48.0, 61.0, 52.0]), Some(61.0));
        assert_eq!(hottest(&[]), None);
    }
}
