use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
"#;

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
    fn the_default_template_parses() {
        let cfg: Config = toml::from_str(DEFAULT_TEMPLATE).unwrap();
        assert_eq!(cfg.interval_secs, 1);
        assert!(cfg.sessions_dir.is_none());
    }
}
