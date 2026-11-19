//! Update check against the project's GitHub releases. The only network
//! request heatlog ever makes to the internet, and only when asked for.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASES: &str = "https://api.github.com/repos/Hami-K/heatlog/releases?per_page=20";
/// Downloads are only ever fetched from this project's own release assets.
const DOWNLOAD_PREFIX: &str = "https://github.com/Hami-K/heatlog/releases/download/";
const CHECKSUMS: &str = "SHA256SUMS.txt";
/// heatlog's own programs, as the release publishes them. An update
/// replaces whichever of these sit next to the running exe - at the same
/// path and under the same name, because that is what taskbar and Start
/// menu pins point at.
const APP_FILES: [&str; 2] = ["heatlog-gui.exe", "heatlog.exe"];
/// The id Inno Setup registers heatlog under, so an in-app update can
/// correct the version Windows shows in "Installed apps".
const UNINSTALL_KEY: &str = concat!(
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\",
    "{4C9F5B2E-18D7-4C44-9A3B-7F1A6E2D5C10}_is1"
);

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: String,
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
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Clone)]
pub struct Available {
    pub version: String,
    pub prerelease: bool,
    /// heatlog's own programs, which an in-app update downloads.
    pub app_files: Vec<Asset>,
    /// The release notes, as written on GitHub.
    pub notes: String,
    /// The checksum list; without it heatlog refuses to install.
    pub checksums_url: Option<String>,
    pub page_url: String,
}

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
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
    Ok(pick(releases, include_prereleases, current_version()).map(describe))
}

/// The highest release the user is allowed to move to - not the next one
/// along. Someone who skipped three versions gets the newest, in one step.
fn pick(releases: Vec<GhRelease>, include_prereleases: bool, current: &str) -> Option<GhRelease> {
    releases
        .into_iter()
        .filter(|r| !r.draft && (include_prereleases || !r.prerelease))
        .filter(|r| is_newer(&r.tag_name, current))
        .reduce(|best, r| {
            if is_newer(&r.tag_name, &best.tag_name) {
                r
            } else {
                best
            }
        })
}

fn describe(release: GhRelease) -> Available {
    Available {
        version: release.tag_name.trim_start_matches('v').to_string(),
        prerelease: release.prerelease,
        app_files: release
            .assets
            .iter()
            .filter(|asset| APP_FILES.contains(&asset.name.as_str()))
            .map(asset_of)
            .collect(),
        checksums_url: release
            .assets
            .iter()
            .find(|asset| asset.name == CHECKSUMS)
            .map(|asset| asset.browser_download_url.clone()),
        notes: release.body.clone(),
        page_url: release.html_url,
    }
}

fn asset_of(asset: &GhAsset) -> Asset {
    Asset {
        name: asset.name.clone(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
    }
}

/// What an in-place update is doing, for the dialog that shows it.
pub enum Progress {
    /// Bytes fetched so far, out of the total to fetch.
    Downloading { got: u64, total: u64 },
    /// Downloads verified; heatlog's files are being replaced.
    Installing,
}

/// Installs an update by replacing heatlog's own files, with no installer
/// and no admin prompt: the files live under the user's own
/// %LOCALAPPDATA%. Every file ends up at exactly the path it had before,
/// which is what keeps taskbar and Start menu pins working.
///
/// Windows cannot overwrite a running exe, so the old one is renamed
/// aside (heatlog-gui.exe.old) and deleted on the next start. If anything
/// fails, the old files are put back, so heatlog always still launches.
pub fn install_in_place(
    available: &Available,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<()> {
    let exe = std::env::current_exe().context("finding heatlog's own folder")?;
    let dir = exe.parent().context("heatlog has no parent folder")?;
    let checksums_url = available.checksums_url.as_ref().with_context(|| {
        format!("this release publishes no {CHECKSUMS}, so heatlog will not install it")
    })?;

    // Only files that are actually here get replaced: a portable copy of
    // heatlog-gui.exe on its own must not grow a CLI it never had.
    let wanted: Vec<&Asset> = available
        .app_files
        .iter()
        .filter(|asset| dir.join(&asset.name).is_file())
        .collect();
    ensure!(
        !wanted.is_empty(),
        "this release publishes no replacement for {}",
        exe.display()
    );
    for url in wanted.iter().map(|asset| &asset.url).chain([checksums_url]) {
        ensure!(
            url.starts_with(DOWNLOAD_PREFIX),
            "refusing to download from outside the project's releases: {url}"
        );
    }

    let sums = get(checksums_url)?
        .into_string()
        .context("reading the checksum list")?;

    let total = wanted.iter().map(|asset| asset.size).sum();
    let mut done = 0u64;
    progress(Progress::Downloading { got: 0, total });
    let mut downloaded = Vec::new();
    for asset in &wanted {
        let body = download(asset, cancel, |got| {
            progress(Progress::Downloading {
                got: done + got,
                total,
            });
        })?;
        verify(&body, &sums, &asset.name)?;
        done += body.len() as u64;
        downloaded.push((dir.join(&asset.name), body));
    }

    progress(Progress::Installing);
    replace_files(&downloaded)?;
    set_installed_version(dir, &available.version);
    Ok(())
}

/// Streams one asset into memory, reporting progress and stopping early if
/// the user cancels.
fn download(asset: &Asset, cancel: &AtomicBool, mut progress: impl FnMut(u64)) -> Result<Vec<u8>> {
    let mut reader = get(&asset.url)?.into_reader();
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        let read = reader
            .read(&mut chunk)
            .with_context(|| format!("downloading {}", asset.name))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
        progress(body.len() as u64);
    }
    Ok(body)
}

/// Swaps every file in one go, or puts them all back. The running exe
/// cannot be deleted, only renamed, so the old copies are left behind for
/// `clean_old_files` to remove on the next start.
fn replace_files(files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    let mut swapped: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (path, bytes) in files {
        let backup = old_path(path);
        let swap = remove_if_present(&backup)
            .and_then(|()| std::fs::rename(path, &backup))
            .and_then(|()| std::fs::write(path, bytes));
        match swap {
            Ok(()) => swapped.push((path.clone(), backup)),
            Err(e) => {
                // Put back what was swapped already, including this file
                // if it got as far as being renamed.
                let _ = std::fs::rename(&backup, path);
                for (path, backup) in swapped.iter().rev() {
                    let _ = std::fs::remove_file(path);
                    let _ = std::fs::rename(backup, path);
                }
                return Err(e).with_context(|| format!("replacing {}", path.display()));
            }
        }
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn old_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

/// Removes the previous version's files, left behind because Windows
/// cannot delete a running program. Call once at start-up.
pub fn clean_old_files() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    for name in APP_FILES {
        let _ = std::fs::remove_file(old_path(&dir.join(name)));
    }
    // A portable copy may have any name at all.
    let _ = std::fs::remove_file(old_path(&exe));
}

/// Corrects the version Windows shows in "Installed apps", so the entry
/// (and the uninstaller behind it) still describes what is installed.
/// Cosmetic, so a failure here does not fail the update.
fn set_installed_version(dir: &Path, version: &str) {
    // Only the copy the installer put there owns that entry: a portable
    // heatlog updating itself must not rewrite someone else's version.
    if !registered_dir().is_some_and(|installed| same_dir(&installed, dir)) {
        return;
    }
    for (name, value) in [
        ("DisplayVersion", version.to_string()),
        ("DisplayName", format!("heatlog {version}")),
    ] {
        reg(&[
            "add",
            UNINSTALL_KEY,
            "/v",
            name,
            "/t",
            "REG_SZ",
            "/d",
            &value,
            "/f",
        ]);
    }
}

/// Where the installed heatlog lives, according to its uninstall entry.
/// None when heatlog was never installed on this account.
fn registered_dir() -> Option<PathBuf> {
    parse_install_location(&reg(&["query", UNINSTALL_KEY, "/v", "InstallLocation"])?)
}

/// reg prints the value as "    InstallLocation    REG_SZ    C:\path\".
fn parse_install_location(output: &str) -> Option<PathBuf> {
    let value = output
        .lines()
        .find(|line| line.contains("InstallLocation"))?
        .split("REG_SZ")
        .nth(1)?
        .trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// Same folder, whatever the spelling or the trailing separator.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// One registry value as a string, or None when the key, the value or
/// `reg` itself is not there. Shared with the PawnIO code, which asks the
/// registry the same questions.
pub(crate) fn reg_value(key: &str, name: &str) -> Option<String> {
    let output = reg(&["query", key, "/v", name])?;
    let value = output
        .lines()
        .find(|line| line.contains(name))?
        .split("REG_SZ")
        .nth(1)?
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn reg(args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    // Without this a console window flashes up over the app.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Release notes for everything between the version the user was on and
/// the one now running, newest first, so one dialog can cover a user who
/// skipped versions.
pub fn notes_since(previous: &str) -> Result<Vec<(String, String)>> {
    let body = ureq::get(RELEASES)
        .set("User-Agent", "heatlog")
        .set("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .context("asking GitHub for releases")?
        .into_string()
        .context("reading GitHub's reply")?;
    let releases: Vec<GhRelease> = serde_json::from_str(&body).context("parsing GitHub's reply")?;
    Ok(notes_between(releases, previous, current_version()))
}

fn notes_between(releases: Vec<GhRelease>, previous: &str, current: &str) -> Vec<(String, String)> {
    let mut picked: Vec<GhRelease> = releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter(|r| is_newer(&r.tag_name, previous) && !is_newer(&r.tag_name, current))
        .collect();
    // Newest first.
    picked.sort_by(|a, b| {
        if is_newer(&a.tag_name, &b.tag_name) {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    picked
        .into_iter()
        .map(|r| (r.tag_name, r.body.trim().to_string()))
        .collect()
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
    use super::{
        is_newer, notes_between, parse_install_location, pick, replace_files, verify, GhRelease,
    };

    #[test]
    fn the_installed_folder_is_read_out_of_regs_output() {
        let output = concat!(
            "\r\nHKEY_CURRENT_USER\\Software\\...\\heatlog_is1\r\n",
            r"    InstallLocation    REG_SZ    C:\Users\me\AppData\Local\Programs\heatlog\",
            "\r\n\r\n"
        );
        assert_eq!(
            parse_install_location(output).unwrap(),
            std::path::Path::new(r"C:\Users\me\AppData\Local\Programs\heatlog\")
        );
        // Never installed: no entry, so nothing to correct.
        assert!(parse_install_location("").is_none());
        assert!(parse_install_location("    InstallLocation    REG_SZ    ").is_none());
    }

    /// The releases GitHub would return, newest first, as it sorts them.
    fn releases() -> Vec<GhRelease> {
        serde_json::from_str(
            r#"[
              {"tag_name":"v0.5.0","html_url":"h/5","draft":true,"prerelease":false,
               "body":"unfinished","assets":[]},
              {"tag_name":"v0.4.1-beta.1","html_url":"h/41b","draft":false,"prerelease":true,
               "body":"beta notes","assets":[]},
              {"tag_name":"v0.4.0","html_url":"h/40","draft":false,"prerelease":false,
               "body":"newest finished","assets":[]},
              {"tag_name":"v0.3.0","html_url":"h/30","draft":false,"prerelease":false,
               "body":"middle","assets":[]},
              {"tag_name":"v0.2.0","html_url":"h/20","draft":false,"prerelease":false,
               "body":"old","assets":[]}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn skipping_several_versions_jumps_straight_to_the_newest() {
        // Three releases behind: the next one along is 0.3.0, but 0.4.0 is
        // what should be offered.
        let picked = pick(releases(), false, "0.2.0").unwrap();
        assert_eq!(picked.tag_name, "v0.4.0");

        // With pre-releases allowed, the newest of those wins instead.
        let picked = pick(releases(), true, "0.2.0").unwrap();
        assert_eq!(picked.tag_name, "v0.4.1-beta.1");

        // A draft is never offered, however new it is.
        assert!(pick(releases(), true, "0.4.1-beta.1").is_none());
        // Already on the newest release: nothing to do.
        assert!(pick(releases(), false, "0.4.0").is_none());
    }

    #[test]
    fn whats_new_covers_every_version_the_user_skipped() {
        let notes = notes_between(releases(), "0.2.0", "0.4.0");
        let tags: Vec<&str> = notes.iter().map(|(tag, _)| tag.as_str()).collect();
        // Newest first, the version left behind excluded, and nothing
        // newer than what is running.
        assert_eq!(tags, ["v0.4.0", "v0.3.0"]);
        assert_eq!(notes[0].1, "newest finished");
    }

    #[test]
    fn a_failed_swap_puts_every_old_file_back() {
        let dir = std::env::temp_dir().join(format!("heatlog-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gui = dir.join("heatlog-gui.exe");
        let cli = dir.join("heatlog.exe");
        std::fs::write(&gui, b"old gui").unwrap();
        std::fs::write(&cli, b"old cli").unwrap();

        // The second file cannot be swapped: nothing is there to rename
        // aside, the same shape of failure as a file Windows has locked.
        let missing = dir.join("gone").join("heatlog.exe");
        let err = replace_files(&[
            (gui.clone(), b"new gui".to_vec()),
            (missing, b"new".to_vec()),
        ]);
        assert!(err.is_err());
        assert_eq!(std::fs::read(&gui).unwrap(), b"old gui");
        assert_eq!(std::fs::read(&cli).unwrap(), b"old cli");

        // And a clean run replaces both, keeping the old ones aside.
        replace_files(&[
            (gui.clone(), b"new gui".to_vec()),
            (cli.clone(), b"new cli".to_vec()),
        ])
        .unwrap();
        assert_eq!(std::fs::read(&gui).unwrap(), b"new gui");
        assert_eq!(std::fs::read(&cli).unwrap(), b"new cli");
        assert_eq!(
            std::fs::read(dir.join("heatlog-gui.exe.old")).unwrap(),
            b"old gui"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

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
