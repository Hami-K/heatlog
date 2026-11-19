//! Whether heatlog may read temperatures through the installed PawnIO
//! driver today.
//!
//! A kernel driver that is safe when shipped can become a liability
//! later: a hole is found, Microsoft adds it to the vulnerable-driver
//! blocklist, or the version on this PC is simply left behind. heatlog
//! must keep protecting users even if heatlog itself stops being
//! maintained, so this check leans on sources other people keep up to
//! date rather than on heatlog's own release schedule:
//!
//! * Windows itself - if the driver will not load, heatlog says so.
//! * The NVD's CVE search, which finds any published advisory naming
//!   PawnIO (GitHub's advisory database cannot help: it is indexed by
//!   package ecosystem, and a Windows driver is in none of them).
//! * PawnIO's own releases, so a newer driver can be offered.
//! * heatlog's own release date, as a dead man's switch.
//! * `driver-safety.json` in heatlog's repository, which can block a
//!   version without an app update.
//!
//! Every answer fails safe: anything unknown, unreachable or unreadable
//! means PawnIO is not used and LibreHardwareMonitor or HWiNFO is.
//! Switching off heatlog's *update* checks does not switch this off; it
//! is a separate setting, and turning this one off means no PawnIO at
//! all.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{data_dir, Config};

/// heatlog's own list. The URL is fixed forever: an old heatlog has to be
/// able to find it years from now.
pub const LIST_URL: &str =
    "https://raw.githubusercontent.com/Hami-K/heatlog/main/driver-safety.json";
/// The only list format this heatlog understands. A newer one is not
/// guessed at - it disables PawnIO instead.
pub const FORMAT: u32 = 1;

const NVD_URL: &str = "https://services.nvd.nist.gov/rest/json/cves/2.0?keywordSearch=PawnIO";
const PAWNIO_LATEST: &str = "https://api.github.com/repos/namazso/PawnIO.Setup/releases/latest";
const HEATLOG_LATEST: &str = "https://api.github.com/repos/Hami-K/heatlog/releases/latest";

/// How long heatlog may go without a release before it assumes nobody is
/// looking after it any more.
const UNMAINTAINED_DAYS: i64 = 365;
/// How old a cached verdict may be before heatlog checks again.
const RECHECK_HOURS: i64 = 24;

/// heatlog's published list of PawnIO versions to avoid.
#[derive(Debug, Clone, Deserialize)]
pub struct List {
    pub format: u32,
    /// The oldest PawnIO heatlog will talk to.
    #[serde(default)]
    pub minimum_safe: Option<String>,
    #[serde(default)]
    pub blocked: Vec<Blocked>,
    /// Advisories already looked at and found not to apply, so one
    /// unrelated CVE mentioning PawnIO cannot disable the feature for
    /// everyone forever.
    #[serde(default)]
    pub ignore_cves: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Blocked {
    pub version: String,
    pub reason: String,
}

/// Everything the decision is made from, so the decision itself is a pure
/// function and every case below has a test.
#[derive(Debug, Default)]
pub struct Facts {
    /// The installed PawnIO version; None when it is not installed.
    pub installed: Option<String>,
    /// heatlog's list, or None when it has never been read.
    pub list: Option<List>,
    /// The list was fetched but could not be understood.
    pub list_unreadable: bool,
    /// Published advisories naming PawnIO.
    pub cves: Vec<String>,
    /// A newer PawnIO than the installed one.
    pub newer_pawnio: Option<String>,
    /// heatlog's own newest release is older than a year.
    pub project_stale: bool,
    /// Windows would not hand over the driver's device although heatlog
    /// is elevated - a blocklisted driver does not load.
    pub driver_refused: bool,
}

/// The verdict, cached on disk so an offline PC keeps the last answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub usable: bool,
    /// Why not, in words a user can act on.
    pub reason: Option<String>,
    /// A newer PawnIO worth offering.
    pub update_to: Option<String>,
    /// When this verdict was reached.
    pub checked: Option<DateTime<Utc>>,
    /// Which PawnIO the verdict is about.
    pub version: Option<String>,
}

impl Status {
    fn no(reason: impl Into<String>, facts: &Facts) -> Self {
        Self {
            usable: false,
            reason: Some(reason.into()),
            update_to: facts.newer_pawnio.clone(),
            checked: Some(Utc::now()),
            version: facts.installed.clone(),
        }
    }
}

/// The verdict, in the order that matters: anything unsafe or unknown
/// beats everything else, and only a complete, current, clean check lets
/// PawnIO be used.
pub fn decide(facts: &Facts, use_when_unmaintained: bool) -> Status {
    let Some(installed) = facts.installed.as_deref() else {
        return Status::no("PawnIO is not installed.", facts);
    };
    if facts.driver_refused {
        return Status::no(
            "Windows would not load the PawnIO driver. That is what Windows does with a driver \
             on its vulnerable-driver blocklist, so heatlog will not use it.",
            facts,
        );
    }
    if facts.list_unreadable {
        return Status::no(
            "heatlog's driver-safety list is in a newer format than this version understands, so \
             it cannot tell whether this driver is safe.",
            facts,
        );
    }
    let Some(list) = facts.list.as_ref() else {
        return Status::no(
            "heatlog has not been able to check yet whether this PawnIO version is safe.",
            facts,
        );
    };
    if list.format != FORMAT {
        return Status::no(
            "heatlog's driver-safety list is in a newer format than this version understands, so \
             it cannot tell whether this driver is safe.",
            facts,
        );
    }
    if let Some(blocked) = list
        .blocked
        .iter()
        .find(|entry| same_version(&entry.version, installed))
    {
        return Status::no(
            format!(
                "PawnIO {installed} is on heatlog's blocked list: {}",
                blocked.reason
            ),
            facts,
        );
    }
    if let Some(minimum) = list.minimum_safe.as_deref() {
        if older(installed, minimum) {
            return Status::no(
                format!(
                    "PawnIO {installed} is older than {minimum}, the oldest version heatlog \
                     will read hardware through."
                ),
                facts,
            );
        }
    }
    let advisory = facts.cves.iter().find(|cve| {
        !list
            .ignore_cves
            .iter()
            .any(|ignored| ignored.eq_ignore_ascii_case(cve))
    });
    if let Some(cve) = advisory {
        return Status::no(
            format!("{cve} is a published advisory naming PawnIO, so heatlog will not use it."),
            facts,
        );
    }
    if facts.project_stale && !use_when_unmaintained {
        return Status::no(
            "heatlog has had no release for over a year, so it is probably no longer maintained \
             and cannot promise this driver is still safe. LibreHardwareMonitor is maintained and \
             keeps PawnIO up to date.",
            facts,
        );
    }
    Status {
        usable: true,
        reason: None,
        update_to: facts.newer_pawnio.clone(),
        checked: Some(Utc::now()),
        version: facts.installed.clone(),
    }
}

/// The verdict to act on right now: the cached one, refreshed when it is
/// stale or about a different PawnIO. Reading is cheap; the network work
/// happens in [`refresh`], which frontends start on a thread.
pub fn status(cfg: &Config) -> Status {
    if !cfg.pawnio_safety_check {
        return Status {
            usable: false,
            reason: Some(
                "The PawnIO driver-safety check is switched off in settings, so heatlog does not \
                 use PawnIO at all."
                    .into(),
            ),
            ..Status::default()
        };
    }
    let installed = crate::sensors::pawnio::installed_version();
    match cached() {
        Some(status) if fresh(&status) && status.version == installed => status,
        // Never checked, or checked too long ago, or a different driver
        // is installed now: no answer means no PawnIO until there is one.
        _ => Status::default(),
    }
}

/// True when a cached verdict is recent enough to act on. An offline PC
/// keeps using the last answer rather than losing the feature, but not
/// forever.
fn fresh(status: &Status) -> bool {
    status
        .checked
        .is_some_and(|checked| (Utc::now() - checked).num_hours() < RECHECK_HOURS)
}

/// Does the whole check and writes the result to the cache. Safe to call
/// on a background thread; it only touches the network and one file.
pub fn refresh(cfg: &Config) -> Status {
    if !cfg.pawnio_safety_check {
        return status(cfg);
    }
    let installed = crate::sensors::pawnio::installed_version();
    let (list, list_unreadable) = fetch_list();
    let facts = Facts {
        driver_refused: installed.is_some()
            && matches!(
                crate::sensors::pawnio::probe(),
                Err(crate::sensors::pawnio::OpenError::DriverRefused)
            ),
        newer_pawnio: installed
            .as_deref()
            .and_then(|have| latest_pawnio().filter(|latest| older(have, latest))),
        installed,
        list,
        list_unreadable,
        cves: advisories(),
        project_stale: project_stale(),
    };
    let status = decide(&facts, cfg.pawnio_if_heatlog_unmaintained);
    // Fetch the official modules here, on this background thread, so the
    // first reading never waits for a download.
    if status.usable {
        let _ = crate::sensors::pawnio::ensure_modules();
    }
    // An offline check must not throw away a good cached verdict: only a
    // check that reached the list is worth remembering.
    if facts.list.is_some() || facts.list_unreadable || facts.installed.is_none() {
        let _ = store(&status);
    }
    status
}

/// Starts a refresh in the background. heatlog carries on with whatever
/// the cache already said.
pub fn refresh_in_background(cfg: &Config) {
    let cfg = cfg.clone();
    std::thread::spawn(move || {
        refresh(&cfg);
    });
}

fn get(url: &str) -> Result<String> {
    ureq::get(url)
        .set("User-Agent", "heatlog")
        .timeout(Duration::from_secs(10))
        .call()
        .with_context(|| format!("fetching {url}"))?
        .into_string()
        .context("reading the reply")
}

/// heatlog's own list. Returns (list, unreadable): a list heatlog cannot
/// parse is a reason to stop, not to ignore.
fn fetch_list() -> (Option<List>, bool) {
    let Ok(body) = get(LIST_URL) else {
        return (None, false);
    };
    match serde_json::from_str::<List>(&body) {
        Ok(list) => (Some(list), false),
        Err(_) => (None, true),
    }
}

/// Every CVE whose text names PawnIO. The NVD keeps this up to date
/// whether or not heatlog is still maintained.
fn advisories() -> Vec<String> {
    let Ok(body) = get(NVD_URL) else {
        return Vec::new();
    };
    parse_nvd(&body)
}

fn parse_nvd(body: &str) -> Vec<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    json["vulnerabilities"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["cve"]["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn latest_pawnio() -> Option<String> {
    let body = get(PAWNIO_LATEST).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    json["tag_name"].as_str().map(str::to_string)
}

/// Whether heatlog itself looks abandoned. Unknown counts as fine: a
/// GitHub outage must not take the feature away.
fn project_stale() -> bool {
    let Ok(body) = get(HEATLOG_LATEST) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else {
        return false;
    };
    json["published_at"]
        .as_str()
        .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
        .is_some_and(|date| (Utc::now() - date.with_timezone(&Utc)).num_days() > UNMAINTAINED_DAYS)
}

fn cache_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("driver-safety-cache.json"))
}

fn cached() -> Option<Status> {
    serde_json::from_str(&fs::read_to_string(cache_path().ok()?).ok()?).ok()
}

fn store(status: &Status) -> Result<()> {
    let path = cache_path()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_string_pretty(status)?)?;
    Ok(())
}

/// Version numbers as Windows writes them: "2.2.0.0" and "2.2.0" are the
/// same driver.
fn numbers(version: &str) -> Vec<u64> {
    let mut parts: Vec<u64> = version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-'])
        .map_while(|part| part.parse().ok())
        .collect();
    while parts.last() == Some(&0) {
        parts.pop();
    }
    parts
}

fn same_version(a: &str, b: &str) -> bool {
    numbers(a) == numbers(b)
}

fn older(version: &str, than: &str) -> bool {
    numbers(version) < numbers(than)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> List {
        List {
            format: FORMAT,
            minimum_safe: Some("2.2.0".into()),
            blocked: vec![Blocked {
                version: "2.1.0.0".into(),
                reason: "it was replaced for a security fix".into(),
            }],
            ignore_cves: Vec::new(),
        }
    }

    fn facts() -> Facts {
        Facts {
            installed: Some("2.2.0.0".into()),
            list: Some(list()),
            ..Facts::default()
        }
    }

    #[test]
    fn a_current_driver_with_a_clean_list_is_used() {
        let status = decide(&facts(), false);
        assert!(status.usable, "{status:?}");
        assert!(status.reason.is_none());
    }

    #[test]
    fn a_blocked_version_is_refused_with_the_published_reason() {
        let status = decide(
            &Facts {
                installed: Some("2.1.0".into()),
                ..facts()
            },
            false,
        );
        assert!(!status.usable);
        assert!(
            status
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("replaced for a security fix")),
            "{status:?}"
        );
    }

    #[test]
    fn a_version_below_the_minimum_is_refused() {
        let status = decide(
            &Facts {
                installed: Some("2.0.0".into()),
                ..facts()
            },
            false,
        );
        assert!(!status.usable);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("older")));
    }

    #[test]
    fn a_published_advisory_disables_the_driver_unless_it_was_ruled_out() {
        let with_cve = Facts {
            cves: vec!["CVE-2030-1234".into()],
            ..facts()
        };
        let status = decide(&with_cve, false);
        assert!(!status.usable);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("CVE-2030-1234")));

        // One unrelated advisory must not kill the feature forever, so
        // the published list can rule it out by name.
        let ruled_out = Facts {
            list: Some(List {
                ignore_cves: vec!["cve-2030-1234".into()],
                ..list()
            }),
            ..Facts {
                cves: vec!["CVE-2030-1234".into()],
                ..facts()
            }
        };
        assert!(decide(&ruled_out, false).usable);
    }

    #[test]
    fn a_driver_windows_refuses_to_load_is_not_used() {
        let status = decide(
            &Facts {
                driver_refused: true,
                ..facts()
            },
            false,
        );
        assert!(!status.usable);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("blocklist")));
    }

    #[test]
    fn an_abandoned_heatlog_stops_using_the_driver_unless_the_user_insists() {
        let stale = Facts {
            project_stale: true,
            ..facts()
        };
        let status = decide(&stale, false);
        assert!(!status.usable);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("no longer maintained")));
        // The user can take that decision back.
        assert!(decide(&stale, true).usable);
    }

    #[test]
    fn a_list_in_an_unknown_format_disables_the_driver_rather_than_guessing() {
        let unreadable = Facts {
            list: None,
            list_unreadable: true,
            ..facts()
        };
        assert!(!decide(&unreadable, false).usable);

        // A list that parses but announces a format from the future is
        // the same situation.
        let future = Facts {
            list: Some(List {
                format: FORMAT + 1,
                ..list()
            }),
            ..facts()
        };
        let status = decide(&future, false);
        assert!(!status.usable);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("newer format")));
    }

    #[test]
    fn with_no_check_ever_completed_the_driver_is_not_used() {
        // Offline on a PC that has never reached the list: fail safe.
        let offline = Facts {
            list: None,
            ..facts()
        };
        let status = decide(&offline, false);
        assert!(!status.usable);
        assert!(status
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("not been able to check")));
    }

    #[test]
    fn an_offline_pc_keeps_using_yesterdays_answer() {
        // What `status` does with the cache: a fresh verdict stands,
        // an old one does not.
        let good = Status {
            usable: true,
            checked: Some(Utc::now() - chrono::Duration::hours(2)),
            ..Status::default()
        };
        assert!(fresh(&good));
        let stale = Status {
            checked: Some(Utc::now() - chrono::Duration::days(3)),
            ..good.clone()
        };
        assert!(!fresh(&stale));
        assert!(!fresh(&Status::default()));
    }

    #[test]
    fn versions_compare_the_way_windows_writes_them() {
        assert!(same_version("2.2.0.0", "2.2.0"));
        assert!(same_version("v2.2.0", "2.2.0.0"));
        assert!(older("2.1.0.0", "2.2.0"));
        assert!(!older("2.2.0.0", "2.2.0"));
        assert!(!older("2.3.0", "2.2.0"));
    }

    #[test]
    fn the_nvd_reply_is_read_for_cve_numbers() {
        let body = r#"{"totalResults":1,"vulnerabilities":[{"cve":{"id":"CVE-2020-14979"}}]}"#;
        assert_eq!(parse_nvd(body), vec!["CVE-2020-14979".to_string()]);
        assert!(parse_nvd(r#"{"totalResults":0,"vulnerabilities":[]}"#).is_empty());
        assert!(parse_nvd("not json at all").is_empty());
    }
}
