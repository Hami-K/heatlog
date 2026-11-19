//! HWiNFO publishes every sensor it reads into a shared-memory block that
//! any user-mode process can open. No driver, no admin rights, nothing for
//! Memory Integrity or an anti-cheat to object to: HWiNFO already owns the
//! privileged part.

use std::mem::size_of;

use anyhow::{bail, ensure, Context, Result};
use windows::core::w;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ,
    MEMORY_BASIC_INFORMATION,
};

use super::cpu_temp::{CpuTempSource, Fan, Reading};

const SIGNATURE: u32 = 0x4857_6953; // "SiWH"
const TEMPERATURE: u32 = 1; // SENSOR_TYPE_TEMP
const FAN: u32 = 3; // SENSOR_TYPE_FAN
const SENSOR_ELEMENT_SIZE: u32 = 264; // 2 DWORDs + 2 x 128-byte names

pub struct HwInfoShm;

impl HwInfoShm {
    /// True when HWiNFO is running with Shared Memory Support enabled.
    pub fn available() -> bool {
        read_block().is_ok_and(|block| parse(&block).cpu_temp_c.is_some())
    }

    /// Everything HWiNFO is sharing, whether or not a CPU temperature is
    /// among it: `read` insists on the temperature, but HWiNFO is also
    /// worth asking for fans alone.
    pub fn read_all(&self) -> Result<Reading> {
        Ok(parse(&read_block()?))
    }
}

impl CpuTempSource for HwInfoShm {
    fn label(&self) -> &'static str {
        "HWiNFO"
    }

    fn read(&mut self) -> Result<Reading> {
        let reading = self.read_all()?;
        if reading.cpu_temp_c.is_none() && reading.fans.is_empty() {
            bail!("no CPU temperature or fan among HWiNFO's sensors");
        }
        Ok(reading)
    }
}

/// Copies the shared-memory block so everything downstream is safe Rust.
fn read_block() -> Result<Vec<u8>> {
    // Unavoidable unsafe: Windows shared memory has no safe std wrapper.
    // The view is opened read-only, copied, then unmapped immediately.
    unsafe {
        let handle = OpenFileMappingW(FILE_MAP_READ.0, false, w!("Global\\HWiNFO_SENS_SM2"))
            .context("HWiNFO is not sharing sensors")?;
        let view = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0);
        if view.Value.is_null() {
            let _ = CloseHandle(handle);
            bail!("could not map HWiNFO's shared memory");
        }
        let mut info = MEMORY_BASIC_INFORMATION::default();
        let queried = VirtualQuery(
            Some(view.Value),
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        );
        // Copy no more than the region actually spans, so a garbled header
        // can never make the parser read past the mapping.
        let len = if queried == 0 {
            0
        } else {
            info.RegionSize.min(16 << 20)
        };
        let bytes = if len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(view.Value.cast::<u8>(), len).to_vec()
        };
        let _ = UnmapViewOfFile(view);
        let _ = CloseHandle(handle);
        ensure!(!bytes.is_empty(), "HWiNFO's shared memory is empty");
        Ok(bytes)
    }
}

struct Layout {
    readings_at: usize,
    reading_size: usize,
    count: usize,
    value_at: usize,
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(raw.try_into().ok()?))
}

fn f64_at(bytes: &[u8], offset: usize) -> Option<f64> {
    let raw = bytes.get(offset..offset + 8)?;
    Some(f64::from_le_bytes(raw.try_into().ok()?))
}

/// Null-terminated ANSI string, as HWiNFO writes its labels.
fn str_at(bytes: &[u8], offset: usize, max: usize) -> Option<&str> {
    let raw = bytes.get(offset..offset + max)?;
    let end = raw.iter().position(|&b| b == 0).unwrap_or(max);
    std::str::from_utf8(&raw[..end]).ok()
}

/// HWiNFO's header has no documented packing and builds differ in whether
/// the 8-byte poll time is padded, so both candidate layouts are tried and
/// the one whose element sizes check out wins.
fn parse_header(bytes: &[u8]) -> Option<Layout> {
    if u32_at(bytes, 0)? != SIGNATURE {
        return None;
    }
    for base in [20usize, 24] {
        if u32_at(bytes, base + 4)? != SENSOR_ELEMENT_SIZE {
            continue;
        }
        let readings_at = u32_at(bytes, base + 12)? as usize;
        let reading_size = u32_at(bytes, base + 16)? as usize;
        let count = u32_at(bytes, base + 20)? as usize;
        // A reading element ends with four doubles (value, min, max, avg),
        // so the current value sits 32 bytes from its end whether the
        // struct is packed or padded.
        if !(300..=1024).contains(&reading_size) {
            continue;
        }
        let value_at = reading_size - 32;
        if readings_at == 0 || count == 0 || count > 10_000 {
            continue;
        }
        return Some(Layout {
            readings_at,
            reading_size,
            count,
            value_at,
        });
    }
    None
}

/// How well a label names the whole-package CPU temperature. Higher wins;
/// `None` means the reading is not a CPU temperature at all.
fn score(label: &str) -> Option<u8> {
    let label = label.to_ascii_lowercase();
    if label.contains("cpu package") {
        Some(4)
    } else if label.contains("tctl") || label.contains("tdie") {
        Some(3)
    } else if label.contains("core max") {
        Some(2)
    } else if label.contains("cpu") {
        Some(1)
    } else {
        None
    }
}

pub fn parse(bytes: &[u8]) -> Reading {
    let Some(layout) = parse_header(bytes) else {
        return Reading::default();
    };
    let mut best: Option<(u8, f32)> = None;
    let mut fans = Vec::new();
    for i in 0..layout.count {
        let at = layout.readings_at + i * layout.reading_size;
        let Some(kind) = u32_at(bytes, at) else { break };
        // Prefer the name the user may have customised, fall back to the
        // one HWiNFO ships.
        let Some(label) = str_at(bytes, at + 140, 128)
            .filter(|label| !label.is_empty())
            .or_else(|| str_at(bytes, at + 12, 128))
        else {
            continue;
        };
        let Some(value) = f64_at(bytes, at + layout.value_at) else {
            continue;
        };
        match kind {
            TEMPERATURE => {
                let Some(score) = score(label) else { continue };
                let celsius = value as f32;
                if !(0.0..=120.0).contains(&celsius) {
                    continue;
                }
                if best.is_none_or(|(best_score, _)| score > best_score) {
                    best = Some((score, celsius));
                }
            }
            FAN if (1.0..=20_000.0).contains(&value) => fans.push(Fan {
                name: label.to_string(),
                rpm: value as u32,
            }),
            _ => {}
        }
    }
    Reading {
        cpu_temp_c: best.map(|(_, celsius)| celsius),
        cores: Vec::new(),
        fans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a block laid out the way HWiNFO lays one out, so the offset
    /// arithmetic and label preference are covered without HWiNFO being
    /// installed.
    fn fake_block(packed: bool, readings: &[(u32, &str, f64)]) -> Vec<u8> {
        let (reading_size, value_at) = if packed { (316usize, 284) } else { (320, 288) };
        let base = if packed { 20usize } else { 24 };
        let readings_at = 1024usize;
        let mut bytes = vec![0u8; readings_at + readings.len() * reading_size];
        bytes[0..4].copy_from_slice(&SIGNATURE.to_le_bytes());
        bytes[base + 4..base + 8].copy_from_slice(&SENSOR_ELEMENT_SIZE.to_le_bytes());
        bytes[base + 12..base + 16].copy_from_slice(&(readings_at as u32).to_le_bytes());
        bytes[base + 16..base + 20].copy_from_slice(&(reading_size as u32).to_le_bytes());
        bytes[base + 20..base + 24].copy_from_slice(&(readings.len() as u32).to_le_bytes());
        for (i, (kind, label, value)) in readings.iter().enumerate() {
            let at = readings_at + i * reading_size;
            bytes[at..at + 4].copy_from_slice(&kind.to_le_bytes());
            bytes[at + 12..at + 12 + label.len()].copy_from_slice(label.as_bytes());
            bytes[at + value_at..at + value_at + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn prefers_the_package_reading_over_other_cpu_temperatures() {
        for packed in [true, false] {
            let block = fake_block(
                packed,
                &[
                    (TEMPERATURE, "CPU Core #1", 61.0),
                    (6, "CPU Package Power", 45.0), // a power reading, not a temperature
                    (TEMPERATURE, "CPU Package", 73.0),
                    (TEMPERATURE, "GPU Temperature", 80.0),
                ],
            );
            assert_eq!(parse(&block).cpu_temp_c, Some(73.0), "packed: {packed}");
        }
    }

    #[test]
    fn falls_back_to_any_cpu_temperature() {
        let block = fake_block(true, &[(TEMPERATURE, "CPU Core #3", 58.5)]);
        assert_eq!(parse(&block).cpu_temp_c, Some(58.5));
    }

    #[test]
    fn rejects_rubbish() {
        assert_eq!(parse(&[0u8; 64]).cpu_temp_c, None);
        // A plausible header, but nothing that names a CPU temperature.
        let block = fake_block(true, &[(TEMPERATURE, "Motherboard", 35.0)]);
        assert_eq!(parse(&block).cpu_temp_c, None);
    }

    #[test]
    fn fans_come_back_with_their_names() {
        let block = fake_block(
            true,
            &[
                (TEMPERATURE, "CPU Package", 71.0),
                (FAN, "CPU Fan", 3100.0),
                (FAN, "GPU Fan", 2750.0),
                (FAN, "Unplugged Fan", 0.0),
            ],
        );
        let reading = parse(&block);
        assert_eq!(reading.cpu_temp_c, Some(71.0));
        // The stopped fan is dropped rather than reported as 0 RPM.
        assert_eq!(reading.fans.len(), 2);
        assert_eq!(reading.fans[0].name, "CPU Fan");
        assert_eq!(reading.fastest_fan(), Some(3100));
    }
}
