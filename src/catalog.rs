use std::collections::{BTreeMap, HashSet};

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
    /// Installer exit code (as a string key) -> what it means, shown on failure.
    #[serde(default)]
    pub exit_codes: BTreeMap<String, String>,
    pub source: Source,
    /// Required for every source except `msstore`, which winget installs.
    #[serde(default)]
    pub installer: Option<Installer>,
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
    /// A text or JSON page that names the latest version, for projects that
    /// publish one alongside versioned download URLs.
    Page {
        url: String,
        /// Capture group 1 = the version, matched against the page body. An
        /// optional group named `file` fills `{file}` in `url_template`.
        version_regex: String,
        /// Download URL with `{version}`, and `{file}` if the regex captures it.
        url_template: String,
        /// Checksum file URL with `{version}`: one bare hash, or `<hash>  <file>` lines.
        #[serde(default)]
        sha256_url: Option<String>,
    },
    /// Proton's `version.json` release feeds.
    Proton {
        url: String,
        #[serde(default = "default_channel")]
        channel: String,
        /// Picks the file by its `Platform` in multi-platform feeds, e.g. "windows/x64".
        #[serde(default)]
        platform: Option<String>,
        /// Download URL with `{version}`, for feeds whose `Url` is unversioned.
        #[serde(default)]
        url_template: Option<String>,
    },
    Direct {
        url: String,
        #[serde(default = "default_direct_version")]
        version: String,
        #[serde(default)]
        sha256: Option<String>,
    },
    /// A Microsoft Store product, installed with winget; nothing is downloaded.
    MsStore {
        /// Store product id, e.g. `9NP83LWLPZ9K`.
        id: String,
    },
}

fn default_channel() -> String {
    "Stable".into()
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
    /// An MSIX package, installed for the current user with Add-AppxPackage.
    Msix,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Detect {
    Registry {
        display_name: String,
        #[serde(default)]
        version_regex: Option<String>,
        /// Exe relative to the install folder whose embedded version is used
        /// instead of DisplayVersion, for installers that leave it stale.
        #[serde(default)]
        exe: Option<String>,
    },
    /// Glob patterns for the app's exe, in priority order; the first match is
    /// the install, and zip/portable updates go into its folder.
    File { paths: Vec<String> },
    /// An MSIX/AppX package installed for the current user, by package family
    /// name (`Name_PublisherId`); the version is the package's.
    Appx { family: String },
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
        for code in app.exit_codes.keys() {
            code.parse::<i32>()
                .with_context(|| format!("app `{}`: exit code `{code}` is not a number", app.id))?;
        }
        match (&app.source, &app.installer) {
            (Source::MsStore { .. }, Some(_)) => {
                bail!(
                    "app `{}`: msstore apps are installed by winget and take no installer",
                    app.id
                )
            }
            (Source::MsStore { .. }, None) if !app.self_updating => {
                bail!("app `{}`: msstore apps must be self_updating", app.id)
            }
            (Source::MsStore { .. }, None) if !matches!(app.detect, Detect::Appx { .. }) => {
                bail!("app `{}`: msstore apps need appx detection", app.id)
            }
            (Source::MsStore { .. }, None) | (_, Some(_)) => {}
            (_, None) => bail!("app `{}`: missing installer", app.id),
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
            Source::Redirect { version_regex, .. } | Source::Page { version_regex, .. } => {
                patterns.push(version_regex);
            }
            Source::Manifest { .. }
            | Source::LastModified { .. }
            | Source::Proton { .. }
            | Source::Direct { .. }
            | Source::MsStore { .. } => {}
        }
        match &app.detect {
            Detect::Registry {
                display_name,
                version_regex,
                ..
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
            Detect::Appx { family } => {
                if !family.contains('_') {
                    bail!(
                        "app `{}`: appx detection needs a package family name (Name_PublisherId), got `{family}`",
                        app.id
                    );
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
    fn sandboxie_plus_tracks_only_the_plus_x64_build() {
        let catalog = load().unwrap();
        let app = catalog
            .apps
            .iter()
            .find(|a| a.id == "sandboxie-plus")
            .unwrap();
        assert!(!app.essential);
        assert!(!app.self_updating);

        let Source::Github { asset, .. } = &app.source else {
            panic!("expected a github source");
        };
        let asset = Regex::new(asset).unwrap();
        assert!(asset.is_match("Sandboxie-Plus-x64-v1.16.4.exe"));
        assert!(asset.is_match("Sandboxie-Plus-x64-v1.16.4a.exe"));
        assert!(!asset.is_match("Sandboxie-Plus-ARM64-v1.16.4.exe"));
        assert!(!asset.is_match("Sandboxie-Classic-x64-v5.71.4.exe"));

        let Detect::Registry { display_name, .. } = &app.detect else {
            panic!("expected registry detection");
        };
        let display_name = Regex::new(display_name).unwrap();
        assert!(display_name.is_match("Sandboxie-Plus v1.16.4"));
        assert!(!display_name.is_match("Sandboxie 5.71.4 (64-bit)"));
    }

    #[test]
    fn factory_is_a_portable_exe_outside_its_data_folder() {
        let catalog = load().unwrap();
        let app = catalog.apps.iter().find(|a| a.id == "factory").unwrap();
        assert!(!app.essential);

        let Source::Github { repo, asset, .. } = &app.source else {
            panic!("expected a github source");
        };
        assert_eq!(repo, "Milkshak3s/factory");
        let asset = Regex::new(asset).unwrap();
        assert!(asset.is_match("factory-1.2.3-windows-x64.exe"));
        assert!(asset.is_match("factory-v1.2.3-windows-amd64.exe"));
        assert!(asset.is_match("factory_1.2.3_windows_x86_64.exe"));
        assert!(asset.is_match("factory-windows-x64.exe"));
        assert!(asset.is_match("factory.exe"));
        assert!(!asset.is_match("factory-1.2.3-windows-x64.msi"));
        assert!(!asset.is_match("factory-1.2.3-windows-arm64.exe"));
        assert!(!asset.is_match("factory-1.2.3-linux-x64"));

        let Some(Installer::Portable { dest, file_name }) = &app.installer else {
            panic!("expected a portable installer");
        };
        assert_eq!(dest, r"%LOCALAPPDATA%\Programs\factory");
        assert_eq!(file_name.as_deref(), Some("factory.exe"));
    }

    #[test]
    fn cpu_z_tracks_only_the_standard_english_setup() {
        let catalog = load().unwrap();
        let app = catalog.apps.iter().find(|a| a.id == "cpu-z").unwrap();
        assert!(!app.essential);
        assert!(!app.self_updating);

        let Source::Page {
            version_regex,
            url_template,
            ..
        } = &app.source
        else {
            panic!("expected a page source");
        };
        let version = Regex::new(version_regex).unwrap();
        let page = r#"<a href="https://download.cpuid.com/cpu-z/cpu-z_2.15-en.exe">SETUP</a>"#;
        assert_eq!(
            version.captures(page).map(|c| c[1].to_owned()).as_deref(),
            Some("2.15")
        );
        assert!(!version.is_match("cpu-z_2.15-rog-en.exe"));
        assert!(!version.is_match("cpu-z_2.15-en.zip"));
        assert_eq!(
            url_template.replace("{version}", "2.15"),
            "https://download.cpuid.com/cpu-z/cpu-z_2.15-en.exe"
        );

        let Detect::Registry { display_name, .. } = &app.detect else {
            panic!("expected registry detection");
        };
        let display_name = Regex::new(display_name).unwrap();
        assert!(display_name.is_match("CPUID CPU-Z 2.15"));
        assert!(!display_name.is_match("CPUID HWMonitor 1.55"));
    }

    #[test]
    fn crystaldiskmark_tracks_only_the_standard_installer() {
        let catalog = load().unwrap();
        let app = catalog
            .apps
            .iter()
            .find(|a| a.id == "crystaldiskmark")
            .unwrap();
        assert!(!app.essential);
        assert!(!app.self_updating);

        let Source::Page { version_regex, .. } = &app.source else {
            panic!("expected a page source");
        };
        let version = Regex::new(version_regex).unwrap();
        let base = "https://sourceforge.net/projects/crystaldiskmark/files";
        let captured = |file: &str| {
            version
                .captures(&format!("<link>{base}/{file}/download</link>"))
                .map(|c| c[1].to_owned())
        };
        assert_eq!(
            captured("9.0.1/CrystalDiskMark9_0_1.exe").as_deref(),
            Some("9.0.1")
        );
        assert_eq!(
            captured("8.0.4/CrystalDiskMark8_0_4c.exe").as_deref(),
            Some("8.0.4")
        );
        // Hotfix folders keep their letter, since the download URL is built from it.
        assert_eq!(
            captured("8.0.4c/CrystalDiskMark8_0_4c.exe").as_deref(),
            Some("8.0.4c")
        );
        assert_eq!(captured("9.0.1/CrystalDiskMark9_0_1.zip"), None);
        assert_eq!(captured("9.0.1/CrystalDiskMark9_0_1Shizuku.exe"), None);

        let Detect::Registry { display_name, .. } = &app.detect else {
            panic!("expected registry detection");
        };
        let display_name = Regex::new(display_name).unwrap();
        assert!(display_name.is_match("CrystalDiskMark 9.0.1"));
        assert!(!display_name.is_match("CrystalDiskInfo 9.7.0"));
    }

    #[test]
    fn geforce_driver_is_optional_and_detects_only_the_display_driver() {
        let catalog = load().unwrap();
        let app = catalog
            .apps
            .iter()
            .find(|a| a.id == "geforce-driver")
            .unwrap();
        // It only installs on some GPUs, so it's never preselected.
        assert!(!app.essential);
        assert!(!app.self_updating);

        let Detect::Registry { display_name, .. } = &app.detect else {
            panic!("expected registry detection");
        };
        let display_name = Regex::new(display_name).unwrap();
        assert!(display_name.is_match("NVIDIA Graphics Driver 566.36"));
        assert!(!display_name.is_match("NVIDIA HD Audio Driver 1.4.2.6"));
        assert!(!display_name.is_match("NVIDIA PhysX System Software 9.23.1019"));
        assert!(!display_name.is_match("NVIDIA app 11.0.1.189"));
    }

    #[test]
    fn nvidia_app_only_installs_and_detects_only_itself() {
        let catalog = load().unwrap();
        let app = catalog.apps.iter().find(|a| a.id == "nvidia-app").unwrap();
        assert!(!app.essential);
        assert!(app.self_updating);

        let Detect::Registry { display_name, .. } = &app.detect else {
            panic!("expected registry detection");
        };
        let display_name = Regex::new(display_name).unwrap();
        assert!(display_name.is_match("NVIDIA app 11.0.1.189"));
        assert!(display_name.is_match("NVIDIA App 11.0.1.189"));
        assert!(!display_name.is_match("NVIDIA Application Profiles"));
        assert!(!display_name.is_match("NVIDIA GeForce Experience 3.28.0.417"));
        assert!(!display_name.is_match("NVIDIA Graphics Driver 566.36"));
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

    #[test]
    fn parses_msstore_source_and_appx_detect() {
        let text = r#"
            [[app]]
            id = "store-app"
            name = "Store App"
            self_updating = true
            source = { type = "msstore", id = "9NP83LWLPZ9K" }
            detect = { type = "appx", family = "Vendor.App_abc123" }
        "#;
        let catalog = parse(text).unwrap();
        let app = &catalog.apps[0];
        assert!(matches!(&app.source, Source::MsStore { id } if id == "9NP83LWLPZ9K"));
        assert!(matches!(&app.detect, Detect::Appx { family } if family == "Vendor.App_abc123"));
        assert!(app.installer.is_none());
    }

    #[test]
    fn rejects_invalid_store_entries() {
        let entry = |extra: &str, detect: &str| {
            format!(
                r#"
                [[app]]
                id = "a"
                name = "A"
                source = {{ type = "msstore", id = "9NP83LWLPZ9K" }}
                detect = {detect}
                {extra}
                "#
            )
        };
        let appx = r#"{ type = "appx", family = "Vendor.App_abc123" }"#;
        let installer = r#"installer = { type = "exe" }"#;
        assert!(parse(&entry("self_updating = true", appx)).is_ok());
        // The Store updates these apps, and winget installs them.
        assert!(parse(&entry("", appx)).is_err());
        let with_installer = format!("self_updating = true\n{installer}");
        assert!(parse(&entry(&with_installer, appx)).is_err());
        // A package name without the publisher id is not a family name.
        let bare = r#"{ type = "appx", family = "Vendor.App" }"#;
        assert!(parse(&entry("self_updating = true", bare)).is_err());
        let managed = r#"{ type = "managed" }"#;
        assert!(parse(&entry("self_updating = true", managed)).is_err());
    }

    #[test]
    fn downloads_still_need_an_installer() {
        let text = r#"
            [[app]]
            id = "a"
            name = "A"
            source = { type = "direct", url = "https://x/a.exe", version = "1" }
        "#;
        assert!(parse(text).is_err());
    }
}
