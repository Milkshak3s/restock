use anyhow::{Context, Result};
use regex::Regex;
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::catalog::Source;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub file_name: String,
    pub sha256: Option<String>,
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("restock/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub fn resolve(client: &Client, source: &Source) -> Result<Release> {
    match source {
        Source::Github { repo, asset, version_regex } => {
            github_latest(client, repo, asset, version_regex.as_deref())
        }
        Source::Manifest { url } => {
            #[derive(Deserialize)]
            struct Manifest {
                version: String,
                url: String,
                sha256: Option<String>,
            }
            let m: Manifest = client.get(url).send()?.error_for_status()?.json()?;
            Ok(Release {
                file_name: file_name_from_url(&m.url),
                version: m.version,
                url: m.url,
                sha256: m.sha256,
            })
        }
        Source::Direct { url, version, sha256 } => Ok(Release {
            version: version.clone(),
            url: url.clone(),
            file_name: file_name_from_url(url),
            sha256: sha256.clone(),
        }),
    }
}

fn github_latest(client: &Client, repo: &str, asset: &str, version_regex: Option<&str>) -> Result<Release> {
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
        .get(format!("https://api.github.com/repos/{repo}/releases/latest"))
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
        Some(pattern) => Regex::new(pattern)?
            .captures(&release.tag_name)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
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
