use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub interval_secs: u64,
    pub sessions_dir: Option<PathBuf>,
    pub games: Vec<String>,
    pub lhm_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_secs: 1,
            sessions_dir: None,
            games: Vec::new(),
            lhm_url: "http://localhost:8085/data.json".to_string(),
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

# Process names for `heatlog record --auto`. Recording starts when one of
# these processes appears and stops when it exits. Find the exact name in
# Task Manager: Details tab, "Name" column, while the game is running.
# Example: games = ["RainbowSix.exe"]
games = []

# LibreHardwareMonitor web server endpoint, used for CPU temperature when
# LibreHardwareMonitor is running with "Remote Web Server" enabled.
lhm_url = "http://localhost:8085/data.json"
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
