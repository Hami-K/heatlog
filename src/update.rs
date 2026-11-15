//! Update check against the project's GitHub releases. The only network
//! request heatlog ever makes to the internet, and only when asked for.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

const RELEASES: &str = "https://api.github.com/repos/Hami-K/heatlog/releases?per_page=20";

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
pub struct Available {
    pub version: String,
    pub prerelease: bool,
    /// The installer, when the release ships one.
    pub installer_url: Option<String>,
    pub page_url: String,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Returns the newest release that beats the running version, or None when
/// heatlog is up to date.
pub fn check(include_prereleases: bool) -> Result<Option<Available>> {
    let body = ureq::get(RELEASES)
        .set("User-Agent", "heatlog")
        .set("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .context("asking GitHub for releases")?
        .into_string()
        .context("reading GitHub's reply")?;
    let releases: Vec<GhRelease> = serde_json::from_str(&body).context("parsing GitHub's reply")?;

    let best = releases
        .into_iter()
        .filter(|r| !r.draft && (include_prereleases || !r.prerelease))
        .filter(|r| is_newer(&r.tag_name, current_version()))
        .max_by(|a, b| {
            if is_newer(&a.tag_name, &b.tag_name) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        });

    Ok(best.map(|release| Available {
        version: release.tag_name.trim_start_matches('v').to_string(),
        prerelease: release.prerelease,
        installer_url: release
            .assets
            .iter()
            .find(|asset| asset.name.ends_with("-setup.exe"))
            .map(|asset| asset.browser_download_url.clone()),
        page_url: release.html_url,
    }))
}

/// Downloads the installer and starts it. The installer is per-user, so it
/// replaces the files without an admin prompt; heatlog should exit after
/// this so the files are not in use.
pub fn run_installer(url: &str) -> Result<PathBuf> {
    let mut body = Vec::new();
    ureq::get(url)
        .set("User-Agent", "heatlog")
        .timeout(std::time::Duration::from_secs(300))
        .call()
        .context("downloading the installer")?
        .into_reader()
        .read_to_end(&mut body)
        .context("saving the installer")?;
    if body.len() < 100_000 {
        bail!("the downloaded installer looks truncated");
    }
    let path = std::env::temp_dir().join("heatlog-setup.exe");
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    std::process::Command::new(&path)
        .spawn()
        .with_context(|| format!("starting {}", path.display()))?;
    Ok(path)
}

/// Compares dotted numeric versions, ignoring a leading "v". Unparsable
/// parts count as 0, so a tag like "v0.2.0-rc1" still orders sensibly.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|part| part.parse().unwrap_or(0))
            .take(3)
            .collect()
    };
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..3 {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        // A 10-ahead minor must not lose to a bigger patch number.
        assert!(is_newer("0.2.0", "0.1.99"));
    }
}
