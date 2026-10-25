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
    pub sha512: Option<String>,
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
                sha512: None,
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
                sha512: None,
            })
        }
        Source::Redirect { url, version_regex } => follow_redirects(url, version_regex),
        Source::Proton {
            url,
            channel,
            platform,
            url_template,
        } => proton_latest(
            client,
            url,
            channel,
            platform.as_deref(),
            url_template.as_deref(),
        ),
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
                sha512: None,
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
            sha512: None,
        }),
        // winget reports Store versions as "Unknown"; these apps are
        // self-updating, so the latest version is never compared.
        Source::MsStore { id } => Ok(Release {
            version: STORE_VERSION.into(),
            url: format!("https://apps.microsoft.com/detail/{id}"),
            file_name: String::new(),
            sha256: None,
            sha512: None,
        }),
    }
}

/// What the Latest column shows for Microsoft Store apps.
pub const STORE_VERSION: &str = "Microsoft Store";

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
        sha512: None,
    })
}

/// Reads one of Proton's `version.json` feeds: the newest release in
/// `channel`, its file for `platform` (multi-platform feeds), and its SHA-512.
fn proton_latest(
    client: &Client,
    url: &str,
    channel: &str,
    platform: Option<&str>,
    url_template: Option<&str>,
) -> Result<Release> {
    let feed: Value = client.get(url).send()?.error_for_status()?.json()?;
    proton_release(&feed, channel, platform, url_template).with_context(|| format!("reading {url}"))
}

fn proton_release(
    feed: &Value,
    channel: &str,
    platform: Option<&str>,
    url_template: Option<&str>,
) -> Result<Release> {
    let releases = feed
        .get("Releases")
        .and_then(Value::as_array)
        .context("no Releases list")?;
    let release = releases
        .iter()
        .find(|r| {
            r.get("CategoryName")
                .and_then(Value::as_str)
                .is_some_and(|c| c.eq_ignore_ascii_case(channel))
        })
        .with_context(|| format!("no {channel} release"))?;
    let version = release
        .get("Version")
        .and_then(Value::as_str)
        .context("release has no Version")?
        .to_owned();

    // `File` is an object in some feeds and a list in others; the CLI's is `Files`.
    let files: Vec<&Value> = match release.get("File").or_else(|| release.get("Files")) {
        Some(Value::Array(list)) => list.iter().collect(),
        Some(file @ Value::Object(_)) => vec![file],
        _ => bail!("release {version} lists no files"),
    };
    let file = match platform {
        Some(p) => files
            .into_iter()
            .find(|f| f.get("Platform").and_then(Value::as_str) == Some(p))
            .with_context(|| format!("release {version} has no {p} file"))?,
        None => files
            .first()
            .copied()
            .with_context(|| format!("release {version} lists no files"))?,
    };
    let download = match url_template {
        Some(template) => template.replace("{version}", &version),
        None => file
            .get("Url")
            .and_then(Value::as_str)
            .context("file has no Url")?
            .to_owned(),
    };
    let sha512 = ["Sha512CheckSum", "Sha512Checksum"]
        .iter()
        .find_map(|key| file.get(*key).and_then(Value::as_str))
        .map(str::to_owned);

    Ok(Release {
        version,
        file_name: file_name_from_url(&download).replace("%20", " "),
        url: download,
        sha256: None,
        sha512,
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
    fn proton_feed_picks_channel_platform_and_template() {
        // Trimmed from proton.me/download/mail/windows/version.json and
        // .../drive/cli/version.json.
        let mail = serde_json::json!({ "Releases": [
            { "CategoryName": "Alpha", "Version": "1.16.0",
              "File": [{ "Url": "https://p/mail/ProtonMail-desktop.exe", "Sha512CheckSum": "aa" }] },
            { "CategoryName": "Stable", "Version": "1.14.0",
              "File": [{ "Url": "https://p/mail/ProtonMail-desktop.exe", "Sha512CheckSum": "bb" }] },
        ]});
        let r = proton_release(
            &mail,
            "Stable",
            None,
            Some("https://p/mail/{version}/ProtonMail-desktop.exe"),
        )
        .unwrap();
        assert_eq!(r.version, "1.14.0");
        assert_eq!(r.url, "https://p/mail/1.14.0/ProtonMail-desktop.exe");
        assert_eq!(r.sha512.as_deref(), Some("bb"));

        let drive = serde_json::json!({ "Releases": [
            { "CategoryName": "Stable", "Version": "3.0.9",
              "File": { "Url": "https://p/drive/Proton%20Drive%20Setup%203.0.9.exe", "Sha512Checksum": "cc" } },
        ]});
        let r = proton_release(&drive, "Stable", None, None).unwrap();
        assert_eq!(r.file_name, "Proton Drive Setup 3.0.9.exe");
        assert_eq!(r.sha512.as_deref(), Some("cc"));

        let cli = serde_json::json!({ "Releases": [
            { "CategoryName": "Stable", "Version": "0.9.0", "Files": [
                { "Url": "https://p/cli/linux-x64/proton-drive", "Platform": "linux/x64" },
                { "Url": "https://p/cli/windows-x64/proton-drive.exe", "Platform": "windows/x64" },
            ]},
        ]});
        let r = proton_release(&cli, "Stable", Some("windows/x64"), None).unwrap();
        assert_eq!(r.url, "https://p/cli/windows-x64/proton-drive.exe");
        assert!(proton_release(&cli, "Beta", None, None).is_err());
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
