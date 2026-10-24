use regex::Regex;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::catalog::{AppEntry, Detect};
use crate::state::State;
use crate::version;

pub struct UninstallEntry {
    pub display_name: String,
    pub display_version: Option<String>,
}

const UNINSTALL_KEYS: &[(winreg::HKEY, &str)] = &[
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
    (
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
    (
        HKEY_CURRENT_USER,
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    ),
];

/// Everything listed under "Installed apps", machine-wide and per-user.
pub fn uninstall_entries() -> Vec<UninstallEntry> {
    let mut entries = Vec::new();
    for &(hive, path) in UNINSTALL_KEYS {
        let Ok(key) = RegKey::predef(hive).open_subkey(path) else {
            continue;
        };
        for name in key.enum_keys().flatten() {
            let Ok(sub) = key.open_subkey(&name) else {
                continue;
            };
            let Ok(display_name) = sub.get_value::<String, _>("DisplayName") else {
                continue;
            };
            entries.push(UninstallEntry {
                display_name,
                display_version: sub.get_value("DisplayVersion").ok(),
            });
        }
    }
    entries
}

pub fn installed_version(
    app: &AppEntry,
    entries: &[UninstallEntry],
    state: &State,
) -> Option<String> {
    match &app.detect {
        Detect::Registry {
            display_name,
            version_regex,
        } => {
            let re = Regex::new(display_name).ok()?;
            let version_re = version_regex.as_deref().and_then(|p| Regex::new(p).ok());
            let entry = entries.iter().find(|e| re.is_match(&e.display_name))?;
            let raw = entry.display_version.as_deref().unwrap_or("unknown");
            let version = version_re
                .and_then(|vr| version::capture(&vr, raw))
                .unwrap_or_else(|| raw.to_owned());
            Some(version)
        }
        Detect::Managed => state.installed.get(&app.id).cloned(),
    }
}
