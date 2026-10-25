use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use regex::Regex;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::Storage::Packaging::Appx::{
    GetPackagePathByFullName, GetPackagesByPackageFamily,
};
use windows_sys::core::PWSTR;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::catalog::{AppEntry, Detect, expand_env};
use crate::state::State;
use crate::version;

pub struct UninstallEntry {
    pub display_name: String,
    pub display_version: Option<String>,
    pub install_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Installed {
    pub version: String,
    /// Folder the app is installed in, when known.
    pub location: Option<PathBuf>,
}

/// `InstallLocation` when set, otherwise the folder of the uninstaller, which
/// installers like OBS's leave as the only hint.
fn install_dir(key: &RegKey) -> Option<PathBuf> {
    let non_empty = |name: &str| {
        key.get_value::<String, _>(name)
            .ok()
            .map(|v| v.trim().trim_matches('"').to_owned())
            .filter(|v| !v.is_empty())
    };
    if let Some(location) = non_empty("InstallLocation") {
        return Some(PathBuf::from(location));
    }
    let uninstall = key.get_value::<String, _>("UninstallString").ok()?;
    let exe = match uninstall.trim().strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => uninstall.trim(),
    };
    // msiexec-based uninstallers say nothing about the app's folder.
    if !exe.to_ascii_lowercase().ends_with(".exe") || exe.to_ascii_lowercase().contains("msiexec") {
        return None;
    }
    Some(Path::new(exe).parent()?.to_path_buf()).filter(|dir| dir.is_dir())
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
                install_dir: install_dir(&sub),
            });
        }
    }
    entries
}

pub fn installed(app: &AppEntry, entries: &[UninstallEntry], state: &State) -> Option<Installed> {
    let recorded = || state.installed.get(&app.id).cloned();
    match &app.detect {
        Detect::Registry {
            display_name,
            version_regex,
            exe,
        } => {
            let re = Regex::new(display_name).ok()?;
            let version_re = version_regex.as_deref().and_then(|p| Regex::new(p).ok());
            let entry = entries.iter().find(|e| re.is_match(&e.display_name))?;
            let from_exe = exe
                .as_deref()
                .zip(entry.install_dir.as_deref())
                .and_then(|(exe, dir)| file_version(&dir.join(exe)));
            let version = from_exe.unwrap_or_else(|| {
                let raw = entry.display_version.as_deref().unwrap_or("unknown");
                version_re
                    .and_then(|vr| version::capture(&vr, raw))
                    .unwrap_or_else(|| raw.to_owned())
            });
            Some(Installed {
                version,
                location: entry.install_dir.clone(),
            })
        }
        Detect::File { paths } => {
            let exe = find_first(paths)?;
            Some(Installed {
                version: file_version(&exe)
                    .or_else(recorded)
                    .unwrap_or_else(|| "unknown".into()),
                location: exe.parent().map(Path::to_path_buf),
            })
        }
        Detect::Appx { family } => {
            let full_name = package_full_names(family)
                .into_iter()
                .reduce(|best, name| {
                    let newer = version::is_newer(
                        package_version(&name).unwrap_or_default(),
                        package_version(&best).unwrap_or_default(),
                    );
                    if newer { name } else { best }
                })?;
            Some(Installed {
                version: package_version(&full_name)?.to_owned(),
                location: package_path(&full_name),
            })
        }
        Detect::Managed => Some(Installed {
            version: recorded()?,
            location: None,
        }),
    }
}

fn find_first(patterns: &[String]) -> Option<PathBuf> {
    patterns.iter().find_map(|pattern| {
        glob::glob(&expand_env(pattern))
            .ok()?
            .flatten()
            .find(|p| p.is_file())
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Full names of the packages in `family` installed for the user Restock runs
/// as. An elevated Restock is still that user, so per-user Store apps show up.
fn package_full_names(family: &str) -> Vec<String> {
    let family = wide(family);
    let (mut count, mut len) = (0u32, 0u32);
    // SAFETY: the first call only reports the sizes; the second gets buffers
    // of exactly those sizes, and each returned name points into `buffer`
    // as a NUL-terminated string.
    unsafe {
        let probe = GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            ptr::null_mut(),
            &mut len,
            ptr::null_mut(),
        );
        if probe != ERROR_INSUFFICIENT_BUFFER || count == 0 {
            return Vec::new();
        }
        let mut names: Vec<PWSTR> = vec![ptr::null_mut(); count as usize];
        let mut buffer = vec![0u16; len as usize];
        let status = GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut len,
            buffer.as_mut_ptr(),
        );
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        names
            .iter()
            .take(count as usize)
            .map(|&name| {
                let len = (0..).take_while(|&i| *name.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(name, len))
            })
            .collect()
    }
}

/// The version in a package full name, `Name_Version_Arch_ResourceId_PublisherId`.
fn package_version(full_name: &str) -> Option<&str> {
    full_name.split('_').nth(1).filter(|v| !v.is_empty())
}

fn package_path(full_name: &str) -> Option<PathBuf> {
    let full_name = wide(full_name);
    let mut len = 0u32;
    // SAFETY: the first call reports the length including the NUL; the second
    // fills a buffer of that length.
    unsafe {
        if GetPackagePathByFullName(full_name.as_ptr(), &mut len, ptr::null_mut())
            != ERROR_INSUFFICIENT_BUFFER
        {
            return None;
        }
        let mut path = vec![0u16; len as usize];
        if GetPackagePathByFullName(full_name.as_ptr(), &mut len, path.as_mut_ptr())
            != ERROR_SUCCESS
        {
            return None;
        }
        path.truncate(path.iter().position(|&c| c == 0).unwrap_or(path.len()));
        Some(PathBuf::from(String::from_utf16_lossy(&path)))
    }
}

/// The `major.minor.build.revision` file version embedded in an exe or dll.
fn file_version(path: &Path) -> Option<String> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let root = [u16::from(b'\\'), 0];
    // SAFETY: `wide` and `root` are NUL-terminated, `data` is sized by
    // GetFileVersionInfoSizeW, and VerQueryValueW points `info` inside `data`
    // with `len` checked before the (unaligned) read.
    unsafe {
        let size = GetFileVersionInfoSizeW(wide.as_ptr(), ptr::null_mut());
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(wide.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut info: *mut c_void = ptr::null_mut();
        let mut len = 0u32;
        if VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) == 0
            || info.is_null()
            || (len as usize) < size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = ptr::read_unaligned(info as *const VS_FIXEDFILEINFO);
        let (ms, ls) = (info.dwFileVersionMS, info.dwFileVersionLS);
        Some(format!(
            "{}.{}.{}.{}",
            ms >> 16,
            ms & 0xffff,
            ls >> 16,
            ls & 0xffff
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exe_file_version() {
        let notepad = Path::new(r"C:\Windows\System32\notepad.exe");
        let version = file_version(notepad).expect("notepad has version info");
        assert_eq!(version.split('.').count(), 4);
    }

    #[test]
    fn finds_first_matching_pattern() {
        let patterns = vec![
            r"C:\definitely\not\here\*.exe".to_owned(),
            r"%SystemRoot%\System32\notepad.ex?".to_owned(),
        ];
        let found = find_first(&patterns).expect("notepad exists");
        assert!(found.ends_with("notepad.exe"));
    }

    #[test]
    fn reads_version_from_package_full_name() {
        let name = "AppleInc.AppleDevices_1.1540.24088.0_x64__nzyj5cx40ttqa";
        assert_eq!(package_version(name), Some("1.1540.24088.0"));
        assert_eq!(package_version("NoVersion"), None);
    }

    #[test]
    fn finds_installed_appx_packages() {
        // Every Windows 11 user has the Settings shell package.
        let names = package_full_names("windows.immersivecontrolpanel_cw5n1h2txyewy");
        let name = names.first().expect("Settings package is installed");
        assert!(package_version(name).is_some(), "{name}");
        assert!(package_path(name).is_some_and(|p| p.is_dir()), "{name}");
        assert!(package_full_names("Not.A.Real.Package_0000000000000").is_empty());
    }
}
