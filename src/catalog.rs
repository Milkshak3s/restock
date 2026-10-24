use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use regex::{Captures, Regex};
use serde::Deserialize;

const CATALOG_TOML: &str = include_str!("../catalog.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    #[serde(rename = "app", default)]
    pub apps: Vec<AppEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub essential: bool,
    /// The app keeps itself current, so Restock only installs it.
    #[serde(default)]
    pub self_updating: bool,
    pub source: Source,
    pub installer: Installer,
    #[serde(default)]
    pub detect: Detect,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Github {
        repo: String,
        asset: String,
        #[serde(default)]
        version_regex: Option<String>,
    },
    Manifest {
        url: String,
        #[serde(default = "default_version_key")]
        version_key: String,
        #[serde(default = "default_url_key")]
        url_key: String,
        #[serde(default = "default_sha256_key")]
        sha256_key: String,
    },
    Redirect {
        url: String,
        version_regex: String,
    },
    LastModified {
        url: String,
    },
    Direct {
        url: String,
        #[serde(default = "default_direct_version")]
        version: String,
        #[serde(default)]
        sha256: Option<String>,
    },
}

fn default_direct_version() -> String {
    "latest".into()
}

fn default_version_key() -> String {
    "version".into()
}

fn default_url_key() -> String {
    "url".into()
}

fn default_sha256_key() -> String {
    "sha256".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Installer {
    Exe {
        #[serde(default)]
        args: Vec<String>,
    },
    Msi {
        #[serde(default)]
        args: Vec<String>,
    },
    Zip {
        dest: String,
        #[serde(default)]
        strip_root: bool,
    },
    Portable {
        dest: String,
        #[serde(default)]
        file_name: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Detect {
    Registry {
        display_name: String,
        #[serde(default)]
        version_regex: Option<String>,
    },
    /// Glob patterns for the app's exe, in priority order; the first match is
    /// the install, and zip/portable updates go into its folder.
    File { paths: Vec<String> },
    #[default]
    Managed,
}

/// Expands `%VAR%` references from the environment, leaving unknown ones as-is.
pub fn expand_env(s: &str) -> String {
    let re = Regex::new(r"%([^%]+)%").expect("static regex");
    re.replace_all(s, |c: &Captures| {
        std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_owned())
    })
    .into_owned()
}

pub fn load() -> Result<Catalog> {
    parse(CATALOG_TOML)
}

fn parse(text: &str) -> Result<Catalog> {
    let catalog: Catalog = toml::from_str(text).context("parsing catalog.toml")?;

    let mut ids = HashSet::new();
    for app in &catalog.apps {
        if !ids.insert(app.id.as_str()) {
            bail!("duplicate app id `{}`", app.id);
        }
        let mut patterns = Vec::new();
        match &app.source {
            Source::Github {
                asset,
                version_regex,
                ..
            } => {
                patterns.push(asset);
                patterns.extend(version_regex);
            }
            Source::Redirect { version_regex, .. } => patterns.push(version_regex),
            Source::Manifest { .. } | Source::LastModified { .. } | Source::Direct { .. } => {}
        }
        match &app.detect {
            Detect::Registry {
                display_name,
                version_regex,
            } => {
                patterns.push(display_name);
                patterns.extend(version_regex);
            }
            Detect::File { paths } => {
                if paths.is_empty() {
                    bail!("app `{}`: file detection needs at least one path", app.id);
                }
                for p in paths {
                    glob::Pattern::new(p)
                        .with_context(|| format!("app `{}`: invalid path pattern `{p}`", app.id))?;
                }
            }
            Detect::Managed => {}
        }
        for p in patterns {
            Regex::new(p).with_context(|| format!("app `{}`: invalid regex `{p}`", app.id))?;
        }
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_valid() {
        let catalog = load().unwrap();
        assert!(!catalog.apps.is_empty());
    }

    #[test]
    fn rejects_duplicate_ids() {
        let text = r#"
            [[app]]
            id = "a"
            name = "A"
            source = { type = "direct", url = "https://x/a.exe", version = "1" }
            installer = { type = "exe" }

            [[app]]
            id = "a"
            name = "A again"
            source = { type = "direct", url = "https://x/a.exe", version = "1" }
            installer = { type = "exe" }
        "#;
        assert!(parse(text).is_err());
    }
}
