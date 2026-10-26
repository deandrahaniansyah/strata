//! Update checks against the project's published GitHub releases.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const REPO_URL: &str = "https://github.com/fikrifirmanf/strata";
const LATEST_API: &str = "https://api.github.com/repos/fikrifirmanf/strata/releases/latest";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub notes: String,
    pub published: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `v1.2.3`, `1.2` and `1.2.3-beta.1` parse; pre-release suffixes are ignored for ordering.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(c), Some(cur)) if c > cur)
}

/// The latest release when it is newer than this build, `None` when this build is current.
pub async fn check() -> Result<Option<Release>> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    let resp = client
        .get(LATEST_API)
        .header("User-Agent", format!("Strata/{CURRENT}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("Can't reach GitHub")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("No releases have been published yet");
    }
    if !resp.status().is_success() {
        bail!("GitHub answered {}", resp.status());
    }
    let latest: GithubRelease = resp.json().await.context("Unexpected answer from GitHub")?;
    if !is_newer(&latest.tag_name, CURRENT) {
        return Ok(None);
    }
    Ok(Some(Release {
        version: latest.tag_name.trim_start_matches(['v', 'V']).to_string(),
        url: latest.html_url,
        notes: latest.body.unwrap_or_default().trim().to_string(),
        published: latest.published_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_not_as_text() {
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta.2", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert_eq!(parse_version("V2.3.4+build.7"), Some((2, 3, 4)));
    }
}
