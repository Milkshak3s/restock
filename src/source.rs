use anyhow::{Context, Result, bail};
use regex::Regex;
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{LAST_MODIFIED, LOCATION};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::Value;

use crate::catalog::Source;
use crate::version;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub file_name: String,
    pub sha256: Option<String>,
}

const MAX_REDIRECTS: usize = 10;

fn builder() -> ClientBuilder {
    Client::builder().user_agent(concat!("restock/", env!("CARGO_PKG_VERSION")))
}

pub fn client() -> Result<Client> {
    Ok(builder().build()?)
}

/// Follows a "latest" download link hop by hop until the URL contains the
/// version, without downloading the file itself.
fn follow_redirects(url: &str, version_regex: &str) -> Result<Release> {
    let client = builder().redirect(Policy::none()).build()?;
    let re = Regex::new(version_regex)?;
    let mut current = url.to_owned();

    for _ in 0..MAX_REDIRECTS {
        if let Some(version) = version::capture(&re, &current) {
            return Ok(Release {
                version,
                file_name: file_name_from_url(&current),
                url: current,
                sha256: None,
            });
        }
        let resp = client.get(&current).send()?;
        let location = resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .with_context(|| {
                format!(
                    "{current} did not redirect to a versioned URL (HTTP {})",
                    resp.status()
                )
            })?;
        current = resp.url().join(location)?.to_string();
    }
    bail!("too many redirects from {url}")
}

pub fn resolve(client: &Client, source: &Source) -> Result<Release> {
    match source {
        Source::Github {
            repo,
            asset,
            version_regex,
        } => github_latest(client, repo, asset, version_regex.as_deref()),
        Source::Manifest {
            url,
            version_key,
            url_key,
            sha256_key,
        } => {
            let m: Value = client.get(url).send()?.error_for_status()?.json()?;
            let field = |key: &str| m.get(key).and_then(Value::as_str).map(str::to_owned);
            let download =
                field(url_key).with_context(|| format!("manifest has no string `{url_key}`"))?;
            Ok(Release {
                version: field(version_key)
                    .with_context(|| format!("manifest has no string `{version_key}`"))?,
                file_name: file_name_from_url(&download),
                url: download,
                sha256: field(sha256_key),
            })
        }
        Source::Redirect { url, version_regex } => follow_redirects(url, version_regex),
        Source::LastModified { url } => {
            let resp = client.head(url).send()?.error_for_status()?;
            let header = resp
                .headers()
                .get(LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .with_context(|| format!("{url} sent no Last-Modified header"))?;
            Ok(Release {
                version: date_version(header)
                    .with_context(|| format!("unrecognised Last-Modified `{header}`"))?,
                url: url.clone(),
                file_name: file_name_from_url(url),
                sha256: None,
            })
        }
        Source::Direct {
            url,
            version,
            sha256,
        } => Ok(Release {
            version: version.clone(),
            url: url.clone(),
            file_name: file_name_from_url(url),
            sha256: sha256.clone(),
        }),
    }
}

fn github_latest(
    client: &Client,
    repo: &str,
    asset: &str,
    version_regex: Option<&str>,
) -> Result<Release> {
    #[derive(Deserialize)]
    struct GhRelease {
        tag_name: String,
        assets: Vec<GhAsset>,
    }
    #[derive(Deserialize)]
    struct GhAsset {
        name: String,
        browser_download_url: String,
        digest: Option<String>,
    }

    let mut req = client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json");
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.bearer_auth(token);
    }
    let release: GhRelease = req.send()?.error_for_status()?.json()?;

    let asset_re = Regex::new(asset)?;
    let found = release
        .assets
        .into_iter()
        .find(|a| asset_re.is_match(&a.name))
        .with_context(|| format!("no asset matching `{asset}` in {repo} {}", release.tag_name))?;

    let version = match version_regex {
        Some(pattern) => version::capture(&Regex::new(pattern)?, &release.tag_name)
            .with_context(|| format!("`{pattern}` did not match tag {}", release.tag_name))?,
        None => release.tag_name.trim_start_matches('v').to_owned(),
    };

    Ok(Release {
        version,
        url: found.browser_download_url,
        file_name: found.name,
        sha256: found
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_owned),
    })
}

fn file_name_from_url(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("download")
        .to_owned()
}

/// Turns an HTTP date (`Thu, 15 Oct 2026 23:34:10 GMT`) into a sortable
/// `2026.10.15` version for downloads that publish no version number.
fn date_version(http_date: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = http_date.split_whitespace().skip(1);
    let day: u32 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == month_name)? + 1;
    let year: u32 = parts.next()?.parse().ok()?;
    Some(format!("{year}.{month:02}.{day:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_dates() {
        assert_eq!(
            date_version("Thu, 15 Oct 2026 23:34:10 GMT").as_deref(),
            Some("2026.10.15")
        );
        assert_eq!(
            date_version("Mon, 02 Feb 2026 00:00:00 GMT").as_deref(),
            Some("2026.02.02")
        );
        assert_eq!(date_version("yesterday"), None);
    }

    #[test]
    fn file_names_ignore_query_strings() {
        assert_eq!(
            file_name_from_url("https://x/files/SysinternalsSuite.zip?a=1"),
            "SysinternalsSuite.zip"
        );
        assert_eq!(file_name_from_url("https://x/"), "download");
    }
}
