use regex::Regex;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::catalog::{AppEntry, Detect};
use crate::state::State;

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
        Detect::Registry { display_name } => {
            let re = Regex::new(display_name).ok()?;
            entries
                .iter()
                .find(|e| re.is_match(&e.display_name))
                .map(|e| {
                    e.display_version
                        .clone()
                        .unwrap_or_else(|| "unknown".into())
                })
        }
        Detect::Managed => state.installed.get(&app.id).cloned(),
    }
}
