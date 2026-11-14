use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use wmi::{COMLibrary, WMIConnection};

/// One row per logical core from the "Processor Information" performance
/// counter set, the same source Task Manager uses for its Speed value.
#[derive(Deserialize)]
#[serde(rename = "Win32_PerfFormattedData_Counters_ProcessorInformation")]
#[serde(rename_all = "PascalCase")]
struct ProcessorPerf {
    name: String,
    percent_processor_performance: u64,
    processor_frequency: u64,
}

pub struct FreqReader {
    wmi: WMIConnection,
}

impl FreqReader {
    pub fn new() -> Result<Self> {
        let com = COMLibrary::new().context("initialising COM")?;
        let wmi = WMIConnection::new(com).context("connecting to WMI")?;
        Ok(Self { wmi })
    }

    /// Current per-core clocks in MHz, ordered by (group, core) index.
    /// `ProcessorFrequency` is the base clock; `PercentProcessorPerformance`
    /// scales it and goes above 100 when the core turbos past base.
    pub fn read_mhz(&self) -> Result<Vec<u64>> {
        let rows: Vec<ProcessorPerf> = self
            .wmi
            .query()
            .context("querying Processor Information counters")?;

        let mut cores: Vec<(Vec<u64>, u64)> = rows
            .iter()
            .filter_map(|row| {
                let idx = parse_instance_name(&row.name)?;
                let mhz = row.processor_frequency * row.percent_processor_performance / 100;
                Some((idx, mhz))
            })
            .collect();
        ensure!(!cores.is_empty(), "no per-core counter instances found");
        cores.sort();
        Ok(cores.into_iter().map(|(_, mhz)| mhz).collect())
    }
}

/// Instance names are "group,core" (e.g. "0,19"); aggregate instances such as
/// "_Total" and "0,_Total" are skipped.
fn parse_instance_name(name: &str) -> Option<Vec<u64>> {
    name.split(',')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}
