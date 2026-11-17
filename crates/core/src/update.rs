//! Whether a newer release is out: the repo's latest release on GitHub, its
//! tag against this build's version.
//!
//! One unauthenticated request to GitHub's API, which answers sixty an hour
//! per address — a day's check uses one. It says nothing about the person
//! asking but what any request does: the address it comes from.

use std::time::Duration;

/// The repo's latest release, as GitHub's API serves it: drafts and
/// pre-releases are never "latest".
pub const LATEST: &str = "https://api.github.com/repos/vestigecounty/thinkboard/releases/latest";

/// A release: its version, without the tag's `v`, and its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

/// The latest release, asked of GitHub. `agent` names the app and its
/// version, which GitHub's API wants of every request.
pub async fn latest(agent: &str) -> Result<Release, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(agent)
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    parse_latest(&body).ok_or_else(|| "GitHub's answer named no release".to_owned())
}

/// A release out of GitHub's answer: its tag (`v0.9.0`) and its page.
pub fn parse_latest(body: &serde_json::Value) -> Option<Release> {
    let flag = |field: &str| body.get(field).and_then(serde_json::Value::as_bool).unwrap_or(false);
    if flag("draft") || flag("prerelease") {
        return None;
    }
    let tag = body.get("tag_name")?.as_str()?.trim();
    let url = body.get("html_url")?.as_str()?.trim();
    let version = tag.strip_prefix('v').unwrap_or(tag);
    (!version.is_empty() && !url.is_empty()).then(|| Release { version: version.to_owned(), url: url.to_owned() })
}

/// Whether `candidate` is a later version than `current`: numbers compared
/// part by part (`0.10.0` after `0.9.0`), and a build marked as on the way to
/// a release (`0.9.0-dev`) before that release. Anything that isn't a version
/// is never newer.
pub fn newer(candidate: &str, current: &str) -> bool {
    let (Some(candidate), Some(current)) = (parse(candidate), parse(current)) else {
        return false;
    };
    candidate > current
}

/// A version as `(numbers, released)`, where a suffix (`-dev`, `-rc.1`) is
/// not yet released and so sorts before the same numbers without one.
fn parse(version: &str) -> Option<(Vec<u64>, bool)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split('+').next()?;
    let (numbers, suffix) = version.split_once('-').map_or((version, None), |(numbers, suffix)| (numbers, Some(suffix)));
    let mut parts: Vec<u64> = numbers.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    while parts.len() < 3 {
        parts.push(0);
    }
    Some((parts, suffix.is_none()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_version_is_newer_and_nothing_else_is() {
        assert!(newer("0.9.0", "0.8.0"));
        assert!(newer("0.10.0", "0.9.0"), "by number, not by letter");
        assert!(newer("1.0", "0.9.9"));
        assert!(newer("v0.9.0", "0.9.0-dev"), "a release after the build that led up to it");
        assert!(!newer("0.8.0", "0.8.0"));
        assert!(!newer("0.8.0", "0.9.0"));
        assert!(!newer("0.9.0-dev", "0.9.0"));
        assert!(!newer("latest", "0.8.0"), "not a version");
    }

    #[test]
    fn githubs_answer_is_read_for_the_release() {
        let answer = serde_json::json!({
            "tag_name": "v0.9.0",
            "html_url": "https://github.com/vestigecounty/thinkboard/releases/tag/v0.9.0",
            "draft": false,
            "prerelease": false,
        });
        assert_eq!(
            parse_latest(&answer),
            Some(Release { version: "0.9.0".to_owned(), url: "https://github.com/vestigecounty/thinkboard/releases/tag/v0.9.0".to_owned() })
        );
        let early = serde_json::json!({ "tag_name": "v1.0.0-rc.1", "html_url": "https://x", "prerelease": true });
        assert_eq!(parse_latest(&early), None, "not one to offer");
        assert_eq!(parse_latest(&serde_json::json!({ "message": "Not Found" })), None);
    }
}
