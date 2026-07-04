//! Startup update check against the GitHub Releases API.
//!
//! Fetches the latest published release tag for the repo and compares it to the
//! running binary's version. The check runs on a background thread so it never
//! blocks the UI, and it fails *silently*: no network, a rate-limit, or a
//! malformed response simply yields "no update known" rather than an error the
//! user has to dismiss. A desktop tool should never nag about being offline.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// GitHub REST endpoint for the newest published (non-draft, non-prerelease)
/// release. Returns 404 if the repo has no releases yet, which we treat as
/// "nothing to report".
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/staehle/TISE/releases/latest";

/// Human-facing releases page we point the user at when an update exists.
pub const RELEASES_PAGE: &str = "https://github.com/staehle/tise/releases/latest";

/// A semantic version triple parsed from a `vX.Y.Z` tag or a Cargo version
/// string. Pre-release/build suffixes are ignored for comparison purposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parse a version out of a string like "v2.1.1", "2.1.1", or "2.1.1-rc1".
    /// Returns `None` if it doesn't look like `MAJOR.MINOR.PATCH`.
    fn parse(raw: &str) -> Option<Version> {
        let trimmed = raw.trim().trim_start_matches(['v', 'V']);
        // Drop any pre-release/build metadata (e.g. "-rc1", "+build").
        let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        // Reject trailing junk like "1.2.3.4".
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

/// The version of the running binary, from Cargo at compile time.
fn current_version() -> Option<Version> {
    Version::parse(env!("CARGO_PKG_VERSION"))
}

/// Result of a completed update check. Only ever constructed for a *newer*
/// upstream version; "up to date" and "check failed" both collapse to no
/// message being sent.
#[derive(Clone, Debug)]
pub struct UpdateAvailable {
    /// The upstream tag as GitHub reported it (e.g. "v2.1.2"), for display.
    pub latest_tag: String,
}

/// Handle held by the app. Poll it once per frame via [`UpdateCheck::poll`].
pub struct UpdateCheck {
    rx: Option<Receiver<UpdateAvailable>>,
    result: Option<UpdateAvailable>,
}

impl UpdateCheck {
    /// Spawn the background check. Returns immediately; the network request runs
    /// on a detached thread. Cheap enough to call once at startup.
    pub fn spawn() -> UpdateCheck {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(found) = check_once() {
                // Receiver may be gone if the app closed mid-check; ignore.
                let _ = tx.send(found);
            }
        });
        UpdateCheck {
            rx: Some(rx),
            result: None,
        }
    }

    /// Non-blocking poll. Once a result arrives it is cached and returned on
    /// every subsequent call. Returns `None` while the check is still running,
    /// failed, or reported that we're up to date.
    pub fn poll(&mut self) -> Option<&UpdateAvailable> {
        if self.result.is_none()
            && let Some(rx) = &self.rx
        {
            match rx.try_recv() {
                Ok(found) => {
                    self.result = Some(found);
                    self.rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    // Thread finished without sending: up to date or failed.
                    self.rx = None;
                }
            }
        }
        self.result.as_ref()
    }
}

/// Perform the blocking HTTP request and comparison. Any failure returns
/// `None`. Runs on a background thread, so blocking here is fine.
fn check_once() -> Option<UpdateAvailable> {
    let current = current_version()?;

    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .build();
    let agent: ureq::Agent = config.into();

    let mut resp = agent
        .get(LATEST_RELEASE_API)
        // GitHub requires a User-Agent; without one it 403s.
        .header("User-Agent", "tise-update-check")
        .header("Accept", "application/vnd.github+json")
        .call()
        .ok()?;

    let json: serde_json::Value = resp.body_mut().read_json().ok()?;
    let tag = json.get("tag_name")?.as_str()?.to_string();

    let latest = Version::parse(&tag)?;
    if latest > current {
        Some(UpdateAvailable { latest_tag: tag })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v_prefixed_tag() {
        assert_eq!(
            Version::parse("v2.1.1"),
            Some(Version {
                major: 2,
                minor: 1,
                patch: 1
            })
        );
    }

    #[test]
    fn parses_bare_version() {
        assert_eq!(
            Version::parse("10.0.3"),
            Some(Version {
                major: 10,
                minor: 0,
                patch: 3
            })
        );
    }

    #[test]
    fn ignores_prerelease_suffix() {
        assert_eq!(Version::parse("2.1.1-rc1"), Version::parse("2.1.1"));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Version::parse("not-a-version"), None);
        assert_eq!(Version::parse("2.1"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse(""), None);
    }

    #[test]
    fn ordering_is_semantic_not_lexical() {
        // 2.10.0 must be newer than 2.9.0 (lexical string compare gets this wrong).
        assert!(Version::parse("2.10.0").unwrap() > Version::parse("2.9.0").unwrap());
        assert!(Version::parse("v2.1.2").unwrap() > Version::parse("v2.1.1").unwrap());
        assert!(Version::parse("3.0.0").unwrap() > Version::parse("2.99.99").unwrap());
    }

    #[test]
    fn equal_versions_are_not_greater() {
        let a = Version::parse("2.1.1").unwrap();
        let b = Version::parse("2.1.1").unwrap();
        assert_eq!(a, b);
        assert!(a <= b);
    }

    #[test]
    fn current_version_parses() {
        // The binary's own version must always be parseable, or the update
        // check silently no-ops forever.
        assert!(current_version().is_some());
    }
}
