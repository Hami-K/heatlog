use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
// No deny_unknown_fields: a newer heatlog writes settings an older one
// has never heard of, and refusing the whole file over one unknown line
// would leave the older copy with no config at all.
#[serde(default)]
pub struct Config {
    pub interval_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions_dir: Option<PathBuf>,
    pub games: Vec<String>,
    pub lhm_url: String,
    /// Check GitHub for a newer heatlog on start-up.
    pub check_updates: bool,
    /// Offer pre-releases as updates too.
    pub include_prereleases: bool,
    /// Delete recordings older than this many days on start-up; 0 is off.
    pub delete_after_days: u64,
    /// Record which programs were busy, so reports can suggest what
    /// caused a slowdown. Names of running programs are personal, so this
    /// can be switched off.
    pub record_processes: bool,
    /// Where the CPU temperature comes from: "auto" takes the best
    /// source available, or name one of "pawnio",
    /// "librehardwaremonitor", "hwinfo".
    pub temp_source: String,
    /// Check published advisories, Windows itself and heatlog's own
    /// blocked-version list before reading hardware through the PawnIO
    /// kernel driver. Switching this off means heatlog never uses
    /// PawnIO. Deliberately separate from `check_updates`: turning off
    /// update checks must not turn off a safety check.
    pub pawnio_safety_check: bool,
    /// Keep using PawnIO even when heatlog itself has had no release for
    /// over a year and so is probably unmaintained.
    pub pawnio_if_heatlog_unmaintained: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_secs: 1,
            sessions_dir: None,
            games: Vec::new(),
            lhm_url: "http://localhost:8085/data.json".to_string(),
            check_updates: true,
            include_prereleases: false,
            delete_after_days: 0,
            record_processes: true,
            temp_source: "auto".to_string(),
            pawnio_safety_check: true,
            pawnio_if_heatlog_unmaintained: false,
        }
    }
}

impl Config {
    /// Where session files live: the config override if set, else
    /// `<app data>\heatlog\sessions`.
    pub fn sessions_dir(&self) -> Result<PathBuf> {
        match &self.sessions_dir {
            Some(dir) => Ok(dir.clone()),
            None => Ok(data_dir()?.join("sessions")),
        }
    }
}

pub fn data_dir() -> Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA").context("APPDATA environment variable not set")?;
    Ok(PathBuf::from(appdata).join("heatlog"))
}

const DEFAULT_TEMPLATE: &str = r#"# heatlog configuration

# Seconds between samples while recording.
interval_secs = 1

# Session files are written to a "sessions" folder next to this config file.
# Uncomment to store them somewhere else:
# sessions_dir = "D:\\heatlog-sessions"

# Process names for `heatlog record --auto` and the desktop app's
# auto-record. Recording starts when one of these processes appears and
# stops when it exits. Find the exact name in Task Manager: Details tab,
# "Name" column, while the game is running.
# Example: games = ["RainbowSix.exe"]
games = []

# LibreHardwareMonitor web server endpoint, used for CPU temperature when
# HWiNFO is not sharing sensors.
lhm_url = "http://localhost:8085/data.json"

# Check GitHub for a newer version on start-up, and whether to count
# pre-releases as updates.
check_updates = true
include_prereleases = false

# Delete recordings older than this many days when the app starts.
# 0 keeps them forever.
delete_after_days = 0

# Record which programs were busy during each sample, so reports can
# suggest what caused a slowdown. Program names are written into session
# files; set this to false to record hardware readings only.
record_processes = true

# Where the CPU temperature comes from. "auto" uses the best source
# available: PawnIO when heatlog is running as administrator, otherwise
# LibreHardwareMonitor, otherwise HWiNFO. Set it to "pawnio",
# "librehardwaremonitor" or "hwinfo" to pin one.
temp_source = "auto"

# Before reading hardware through the PawnIO kernel driver, check
# Windows, published advisories and heatlog's own blocked-version list,
# and fall back to LibreHardwareMonitor or HWiNFO if anything looks
# wrong. Switching this off means heatlog never uses PawnIO at all. This
# is not the update check: turning off update checks leaves it on.
pawnio_safety_check = true

# heatlog stops using PawnIO if heatlog itself has had no release for
# over a year, because an unmaintained app cannot promise a kernel driver
# is still safe. Set this to true to keep using it anyway.
pawnio_if_heatlog_unmaintained = false
"#;

/// The version that last ran. Kept beside the config rather than in it:
/// it is heatlog's own bookkeeping, not a setting, and writing the config
/// file would throw away the comments the user reads there.
pub fn last_seen_version() -> Option<String> {
    let path = data_dir().ok()?.join("last-seen-version");
    let version = fs::read_to_string(path).ok()?.trim().to_string();
    (!version.is_empty()).then_some(version)
}

pub fn set_last_seen_version(version: &str) {
    if let Ok(dir) = data_dir() {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("last-seen-version"), version);
    }
}

pub fn config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("config.toml"))
}

/// Loads the config, writing a commented default file on first run.
pub fn load_or_create() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        fs::write(&path, DEFAULT_TEMPLATE)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("Created default config at {}", path.display());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Writes settings changed in the app. The comments from the default
/// template are lost, so a short header points at the documentation.
pub fn save(cfg: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let body = toml::to_string_pretty(cfg).context("serialising settings")?;
    let text = format!("# heatlog configuration - see https://github.com/Hami-K/heatlog\n\n{body}");
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_toml() {
        let cfg = Config {
            games: vec!["RainbowSix.exe".to_string()],
            interval_secs: 2,
            check_updates: false,
            delete_after_days: 30,
            ..Config::default()
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.games, cfg.games);
        assert_eq!(back.interval_secs, 2);
        assert!(!back.check_updates);
        assert_eq!(back.delete_after_days, 30);
    }

    #[test]
    fn an_older_heatlog_still_reads_a_config_written_by_a_newer_one() {
        // The 0.3.0 bug: one unknown key made the whole file unusable.
        let cfg: Config = toml::from_str(
            "interval_secs = 2
something_from_the_future = \"yes\"
",
        )
        .expect("an unknown setting must not throw the config away");
        assert_eq!(cfg.interval_secs, 2);
    }

    #[test]
    fn the_default_template_parses() {
        let cfg: Config = toml::from_str(DEFAULT_TEMPLATE).unwrap();
        assert_eq!(cfg.interval_secs, 1);
        assert!(cfg.sessions_dir.is_none());
        assert_eq!(cfg.temp_source, "auto");
        assert!(cfg.pawnio_safety_check);
        assert!(!cfg.pawnio_if_heatlog_unmaintained);
    }
}
