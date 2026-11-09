//! Update check, platform-free part: version compare, release JSON, the shared state file and the
//! throttle / skip / remind decisions. Pure functions take their time and paths as arguments so
//! they test without a network or the real `%APPDATA%`. The WinHTTP fetch and the prompt live in
//! `update_win`; the editor starts `update_win::spawn_check` on a background thread when it opens.
//!
//! One state file is shared by every Bent Signal plugin and instance
//! (`%APPDATA%\Bent Signal\update.json`). Env overrides for testing: `BENTSIGNAL_UPDATE_TEST_VERSION`
//! (the running version), `BENTSIGNAL_UPDATE_FORCE=1` (ignore throttle, skip and remind) and
//! `BENTSIGNAL_UPDATE_STATE_DIR` (the folder holding `update.json`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const REPO_URL: &str = "https://github.com/DeadlyGoonman/bent-signal/";
pub const LATEST_URL: &str = "https://api.github.com/repos/DeadlyGoonman/bent-signal/releases/latest";
pub const DAY: u64 = 24 * 60 * 60;
/// Longest patch-notes excerpt shown in the prompt.
/// Shortest gap between two network attempts, successful or not.
pub const RETRY: u64 = 60 * 60;
pub const NOTES_MAX: usize = 1200;

/// `major.minor.patch` of a version string (leading `v` and any `-suffix` ignored).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let mut next = |required: bool| -> Option<u64> {
        match it.next() {
            Some(p) => p.parse().ok(),
            None => (!required).then_some(0),
        }
    };
    let v = (next(true)?, next(false)?, next(false)?);
    it.next().is_none().then_some(v)
}

/// Whether `latest` is strictly newer than `current` (false when either does not parse).
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The release fields the prompt needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub body: String,
    pub url: String,
}

/// Fields of GitHub's "latest release" JSON; `None` for drafts, prereleases or a missing tag.
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let flag = |k: &str| v.get(k).and_then(serde_json::Value::as_bool).unwrap_or(false);
    if flag("draft") || flag("prerelease") {
        return None;
    }
    let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let tag = text("tag_name");
    parse_version(&tag)?;
    Some(Release { tag, body: text("body"), url: text("html_url") })
}

/// The shared state file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub enabled: bool,
    /// Unix seconds of the last successful check.
    pub last_check: u64,
    /// Unix seconds of the last network attempt, successful or not.
    pub last_attempt: u64,
    /// Version the user chose to skip (as tagged).
    pub skipped_version: String,
    /// Unix seconds before which no prompt is shown.
    pub remind_after: u64,
    /// The latest release as of `last_check`, so later opens can prompt without the network.
    pub latest_tag: String,
    pub latest_body: String,
    pub latest_url: String,
}

impl Default for State {
    fn default() -> State {
        State {
            enabled: true,
            last_check: 0,
            last_attempt: 0,
            skipped_version: String::new(),
            remind_after: 0,
            latest_tag: String::new(),
            latest_body: String::new(),
            latest_url: String::new(),
        }
    }
}

impl State {
    /// A missing or unreadable file gives the defaults.
    pub fn load(dir: &Path) -> State {
        std::fs::read_to_string(dir.join("update.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let tmp = dir.join(format!("update.json.{}.tmp", std::process::id()));
            if std::fs::write(&tmp, t).is_ok() && std::fs::rename(&tmp, dir.join("update.json")).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// The network is due: never checked, a day passed, or the clock went backwards.
    pub fn check_due(&self, now: u64) -> bool {
        self.latest_tag.is_empty() || self.last_check > now || now - self.last_check >= DAY
    }

    /// A network attempt is allowed: none in the last hour (or the clock went backwards).
    pub fn attempt_due(&self, now: u64) -> bool {
        self.last_attempt == 0 || self.last_attempt > now || now - self.last_attempt >= RETRY
    }

    pub fn store(&mut self, r: &Release, now: u64) {
        self.last_check = now;
        self.latest_tag = r.tag.clone();
        self.latest_body = r.body.clone();
        self.latest_url = r.url.clone();
    }

    /// The cached release to prompt about, if newer than `current` and not skipped or snoozed.
    pub fn pending(&self, current: &str, now: u64, force: bool) -> Option<Release> {
        if !self.enabled && !force {
            return None;
        }
        if !is_newer(&self.latest_tag, current) {
            return None;
        }
        if !force && (self.skipped_version == self.latest_tag || now < self.remind_after) {
            return None;
        }
        Some(Release { tag: self.latest_tag.clone(), body: self.latest_body.clone(), url: self.latest_url.clone() })
    }
}

/// What the user answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Open the download page.
    Open,
    /// Remind me tomorrow.
    Later,
    /// Skip this version.
    Skip,
}

pub fn apply_choice(state: &mut State, choice: Choice, tag: &str, now: u64) {
    match choice {
        Choice::Open => {}
        Choice::Later => state.remind_after = now.saturating_add(DAY),
        Choice::Skip => state.skipped_version = tag.to_string(),
    }
}

/// The page to open: the release's own when it is in this repository, else the latest-release page.
pub fn safe_url(url: &str) -> String {
    if url.starts_with(REPO_URL) && !url.chars().any(|c| c.is_control() || c == ' ') { url.to_string() } else { format!("{REPO_URL}releases/latest") }
}

/// The prompt text.
pub fn message(r: &Release, current: &str) -> String {
    let new = r.tag.trim_start_matches(['v', 'V']);
    let notes = r.body.replace("\r\n", "\n");
    let notes = notes.trim();
    let mut shown: String = notes.chars().take(NOTES_MAX).collect();
    if notes.chars().count() > NOTES_MAX {
        shown.push_str("...");
    }
    format!("Bent Signal {new} is available (you have {current}).\n\n{shown}\n\nYes: open the download page\nNo: remind me tomorrow\nCancel: skip this version")
}

/// The folder holding `update.json`.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("BENTSIGNAL_UPDATE_STATE_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Bent Signal"))
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn running_version() -> String {
    std::env::var("BENTSIGNAL_UPDATE_TEST_VERSION").ok().filter(|v| parse_version(v).is_some()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// `BENTSIGNAL_UPDATE_OFF=1` skips the check entirely (set for cargo test/run in `.cargo/config.toml`,
/// so tests that open an editor never touch the network or the real state file).
pub fn disabled() -> bool {
    std::env::var("BENTSIGNAL_UPDATE_OFF").is_ok_and(|v| v == "1")
}

pub fn forced() -> bool {
    std::env::var("BENTSIGNAL_UPDATE_FORCE").is_ok_and(|v| v == "1")
}

/// The whole decision with the fetch injected: refreshes the cache when due (or forced) and returns
/// the release to prompt about. A failed fetch is a silent `None`; the next attempt waits `RETRY`.
pub fn check(dir: &Path, current: &str, now: u64, force: bool, fetch: impl FnOnce() -> Option<Release>) -> Option<Release> {
    let mut st = State::load(dir);
    if !st.enabled && !force {
        return None;
    }
    if force || (st.check_due(now) && st.attempt_due(now)) {
        // Recorded before the request so a failure (or another instance) does not retry at once.
        st.last_attempt = now;
        st.save(dir);
        if let Some(r) = fetch() {
            st.store(&r, now);
            st.save(dir);
        }
    }
    st.pending(current, now, force)
}

/// Records the user's answer in the shared file (re-read first: another plugin may have written).
pub fn record(dir: &Path, choice: Choice, tag: &str, now: u64) {
    let mut st = State::load(dir);
    apply_choice(&mut st, choice, tag, now);
    st.save(dir);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn rel(tag: &str) -> Release {
        Release { tag: tag.into(), body: "notes".into(), url: format!("{REPO_URL}releases/tag/{tag}") }
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("2.0.1-rc1"), Some((2, 0, 1)));
        assert_eq!(parse_version("x.y"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version(""), None);
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        assert!(!is_newer("junk", "0.3.0"));
        assert!(!is_newer("0.4.0", "junk"));
    }

    #[test]
    fn release_json_fields() {
        let j = r#"{"tag_name":"v0.4.0","body":"- fix\r\n- new","html_url":"https://github.com/DeadlyGoonman/bent-signal/releases/tag/v0.4.0","draft":false,"prerelease":false,"x":1}"#;
        let r = parse_release(j).unwrap();
        assert_eq!((r.tag.as_str(), r.body.as_str()), ("v0.4.0", "- fix\r\n- new"));
        assert!(r.url.ends_with("v0.4.0"));
        assert!(parse_release(&j.replace("\"draft\":false", "\"draft\":true")).is_none());
        assert!(parse_release(&j.replace("\"prerelease\":false", "\"prerelease\":true")).is_none());
        assert!(parse_release(r#"{"body":"x"}"#).is_none());
        assert!(parse_release(r#"{"tag_name":"nightly"}"#).is_none());
        assert!(parse_release("not json").is_none());
        assert_eq!(parse_release(r#"{"tag_name":"1.0.0","body":null}"#).unwrap().body, "");
    }

    #[test]
    fn throttle_skip_and_remind() {
        let mut st = State::default();
        assert!(st.check_due(1000));
        st.store(&rel("v0.4.0"), 1000);
        assert!(!st.check_due(1000 + DAY - 1));
        assert!(st.check_due(1000 + DAY));
        assert!(st.check_due(500), "clock went back");
        assert!(st.pending("0.3.0", 2000, false).is_some());
        assert!(st.pending("0.4.0", 2000, false).is_none(), "not newer");
        apply_choice(&mut st, Choice::Later, "v0.4.0", 2000);
        assert_eq!(st.remind_after, 2000 + DAY);
        assert!(st.pending("0.3.0", 2000 + DAY - 1, false).is_none());
        assert!(st.pending("0.3.0", 2000 + DAY, false).is_some());
        assert!(st.pending("0.3.0", 2001, true).is_some(), "force ignores remind");
        apply_choice(&mut st, Choice::Skip, "v0.4.0", 2000);
        assert!(st.pending("0.3.0", 9 * DAY, false).is_none());
        assert!(st.pending("0.3.0", 9 * DAY, true).is_some(), "force ignores skip");
        st.store(&rel("v0.4.1"), 9 * DAY);
        assert!(st.pending("0.3.0", 9 * DAY, false).is_some(), "a newer version is not skipped");
        st.enabled = false;
        assert!(st.pending("0.3.0", 9 * DAY, false).is_none());
    }

    #[test]
    fn state_file_round_trip_and_check() {
        let dir = std::env::temp_dir().join(format!("bs-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(State::load(&dir), State::default());
        let mut fetched = 0;
        let got = check(&dir, "0.3.0", 1000, false, || {
            fetched += 1;
            Some(rel("v0.4.0"))
        });
        assert_eq!(got.unwrap().tag, "v0.4.0");
        // Within a day: from the cache, no fetch.
        let got = check(&dir, "0.3.0", 2000, false, || {
            fetched += 1;
            None
        });
        assert_eq!((got.unwrap().tag, fetched), ("v0.4.0".to_string(), 1));
        record(&dir, Choice::Skip, "v0.4.0", 2000);
        assert!(check(&dir, "0.3.0", 3000, false, || None).is_none());
        assert!(check(&dir, "0.3.0", 3000, true, || Some(rel("v0.4.0"))).is_some(), "force");
        // A failed fetch when due: silent, the cache (newer than current) still prompts once the
        // skip no longer applies, and the next attempt waits an hour.
        let t = 3000 + 2 * DAY;
        assert!(check(&dir, "0.3.0", t, false, || None).is_none(), "skipped in the cache");
        assert_eq!(State::load(&dir).last_attempt, t);
        assert!(check(&dir, "0.3.0", t + RETRY - 1, false, || panic!("throttled")).is_none());
        let mut st = State::load(&dir);
        st.skipped_version.clear();
        st.save(&dir);
        assert_eq!(check(&dir, "0.3.0", t + 10, false, || panic!("throttled")).unwrap().tag, "v0.4.0");
        let mut n = 0;
        let _ = check(&dir, "0.3.0", t + RETRY, false, || {
            n += 1;
            None
        });
        assert_eq!(n, 1, "an hour later it tries again");
        let mut st = State::load(&dir);
        st.skipped_version = "v0.4.0".into();
        st.save(&dir);
        // Disabled by hand-editing the file.
        let text = std::fs::read_to_string(dir.join("update.json")).unwrap().replace("\"enabled\": true", "\"enabled\": false");
        std::fs::write(dir.join("update.json"), text).unwrap();
        assert!(!State::load(&dir).enabled);
        assert!(check(&dir, "0.3.0", 9 * DAY, false, || panic!("no network when disabled")).is_none());
        // Garbage file: defaults.
        std::fs::write(dir.join("update.json"), "{{").unwrap();
        assert!(State::load(&dir).enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prompt_text_and_url() {
        let mut r = rel("v0.4.0");
        r.body = "a".repeat(2000);
        let m = message(&r, "0.3.0");
        assert!(m.starts_with("Bent Signal 0.4.0 is available (you have 0.3.0).\n\n"));
        assert!(m.contains(&format!("{}...\n\nYes:", "a".repeat(NOTES_MAX))));
        assert!(m.ends_with("Cancel: skip this version"));
        r.body = "short\r\nnotes".into();
        assert!(message(&r, "0.3.0").contains("short\nnotes\n\nYes:"));
        assert_eq!(safe_url(&rel("v1.0.0").url), rel("v1.0.0").url);
        let fallback = format!("{REPO_URL}releases/latest");
        assert_eq!(safe_url("https://evil.example/x"), fallback);
        assert_eq!(safe_url("https://github.com/DeadlyGoonman/bent-signal-evil"), fallback);
        assert_eq!(safe_url(""), fallback);
    }
}
