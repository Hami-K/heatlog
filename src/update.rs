//! Update check against the project's GitHub releases. The only network
//! request heatlog ever makes to the internet, and only when asked for.

use std::path::PathBuf;

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASES: &str = "https://api.github.com/repos/Hami-K/heatlog/releases?per_page=20";
/// Downloads are only ever fetched from this project's own release assets.
const DOWNLOAD_PREFIX: &str = "https://github.com/Hami-K/heatlog/releases/download/";
const CHECKSUMS: &str = "SHA256SUMS.txt";

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
    pub installer: Option<Asset>,
    /// The checksum list; without it heatlog refuses to install.
    pub checksums_url: Option<String>,
    pub page_url: String,
}

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn get(url: &str) -> Result<ureq::Response> {
    ureq::get(url)
        .set("User-Agent", "heatlog")
        .timeout(std::time::Duration::from_secs(300))
        .call()
        .with_context(|| format!("fetching {url}"))
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
        installer: release
            .assets
            .iter()
            .find(|asset| asset.name.ends_with("-setup.exe"))
            .map(|asset| Asset {
                name: asset.name.clone(),
                url: asset.browser_download_url.clone(),
            }),
        checksums_url: release
            .assets
            .iter()
            .find(|asset| asset.name == CHECKSUMS)
            .map(|asset| asset.browser_download_url.clone()),
        page_url: release.html_url,
    }))
}

/// Downloads the installer, checks it against the release's published
/// SHA-256 list, and only then starts it. The installer is per-user, so it
/// replaces the files without an admin prompt; heatlog should exit after
/// this so the files are not in use.
pub fn run_installer(available: &Available) -> Result<PathBuf> {
    let installer = available
        .installer
        .as_ref()
        .context("this release has no installer")?;
    let checksums_url = available.checksums_url.as_ref().with_context(|| {
        format!(
            "this release publishes no {CHECKSUMS}, so heatlog will not install it automatically"
        )
    })?;
    for url in [&installer.url, checksums_url] {
        ensure!(
            url.starts_with(DOWNLOAD_PREFIX),
            "refusing to download from outside the project's releases: {url}"
        );
    }

    let sums = get(checksums_url)?
        .into_string()
        .context("reading the checksum list")?;
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut get(&installer.url)?.into_reader(), &mut body)
        .context("downloading the installer")?;
    verify(&body, &sums, &installer.name)?;

    let path = std::env::temp_dir().join(&installer.name);
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    std::process::Command::new(&path)
        .spawn()
        .with_context(|| format!("starting {}", path.display()))?;
    Ok(path)
}

/// Checks a download against a `sha256sum`-style list. A mismatch or a
/// missing entry is refused: a tampered or truncated installer must never
/// be run.
fn verify(body: &[u8], sums: &str, name: &str) -> Result<()> {
    let expected = sums
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*') == name)
        .map(|(hash, _)| hash.trim().to_ascii_lowercase())
        .with_context(|| format!("{CHECKSUMS} has no entry for {name}"))?;
    let actual = format!("{:x}", Sha256::digest(body));
    if actual != expected {
        bail!("{name} does not match its published checksum, so it was not run");
    }
    Ok(())
}

/// A version split into its numbers and its pre-release tail, so that
/// 0.3.0-beta.2 can be recognised as newer than 0.3.0-beta.1 and older
/// than the 0.3.0 release itself.
fn parts(version: &str) -> ([u64; 3], Option<(String, u64)>) {
    let version = version.trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut numbers = [0u64; 3];
    for (slot, part) in numbers.iter_mut().zip(core.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    // "beta.2" sorts by its number, not as text, so beta.10 beats beta.9.
    let pre = pre.map(|pre| match pre.rsplit_once('.') {
        Some((name, number)) if number.parse::<u64>().is_ok() => {
            (name.to_string(), number.parse().unwrap_or(0))
        }
        _ => (pre.to_string(), 0),
    });
    (numbers, pre)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, a_pre) = parts(candidate);
    let (b, b_pre) = parts(current);
    if a != b {
        return a > b;
    }
    match (a_pre, b_pre) {
        // Same numbers: the finished release beats any pre-release of it.
        (None, None) => false,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => a > b,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, verify};

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

    #[test]
    fn betas_update_to_later_betas_and_to_the_release() {
        assert!(is_newer("v0.3.0-beta.2", "0.3.0-beta.1"));
        assert!(is_newer("v0.3.0-beta.10", "0.3.0-beta.9"));
        assert!(!is_newer("v0.3.0-beta.1", "0.3.0-beta.2"));
        // The finished release supersedes its own pre-releases.
        assert!(is_newer("v0.3.0", "0.3.0-beta.3"));
        assert!(!is_newer("v0.3.0-beta.3", "0.3.0"));
        // And a beta of the next version still beats the last release.
        assert!(is_newer("v0.3.0-beta.1", "0.2.3"));
        assert!(!is_newer("v0.2.3", "0.3.0-beta.1"));
    }

    #[test]
    fn only_an_exact_checksum_match_passes() {
        let body = b"pretend installer";
        // sha256 of the line above, as the workflow would publish it.
        let good = format!("{:x}", sha2::Sha256::digest(body));
        let sums = format!(
            "{good}  heatlog-setup.exe\n\
             0000000000000000000000000000000000000000000000000000000000000000  heatlog.exe\n"
        );
        assert!(verify(body, &sums, "heatlog-setup.exe").is_ok());

        // Tampered download.
        assert!(verify(b"something else", &sums, "heatlog-setup.exe").is_err());
        // Right file, wrong list entry.
        assert!(verify(body, &sums, "heatlog.exe").is_err());
        // No entry at all.
        assert!(verify(body, &sums, "heatlog-gui.exe").is_err());
    }

    use sha2::Digest;
}
