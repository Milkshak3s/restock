//! Which running programs hold files in an install folder open, via the
//! Windows Restart Manager. Used to explain "files in use" installer failures.

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
    RmStartSession,
};

/// Display names of programs using executables or libraries under `dir`.
pub fn programs_using(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_binaries(dir, &mut files);
    if files.is_empty() {
        return Vec::new();
    }
    let wide: Vec<Vec<u16>> = files
        .iter()
        .map(|f| f.as_os_str().encode_wide().chain(Some(0)).collect())
        .collect();
    let ptrs: Vec<*const u16> = wide.iter().map(|w| w.as_ptr()).collect();

    // SAFETY: `ptrs` points into `wide`, which outlives the session; the
    // RM_PROCESS_INFO buffer is sized to the count RmGetList reports, and the
    // session is always ended.
    unsafe {
        let mut session = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        if RmStartSession(&mut session, 0, key.as_mut_ptr()) != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut names = Vec::new();
        if RmRegisterResources(
            session,
            ptrs.len() as u32,
            ptrs.as_ptr(),
            0,
            ptr::null(),
            0,
            ptr::null(),
        ) == ERROR_SUCCESS
        {
            let mut needed = 0u32;
            let mut count = 0u32;
            let mut reasons = 0u32;
            let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
            // The list can grow between calls, so retry while it reports more.
            loop {
                let status = RmGetList(
                    session,
                    &mut needed,
                    &mut count,
                    infos.as_mut_ptr(),
                    &mut reasons,
                );
                if status == ERROR_MORE_DATA {
                    infos.resize_with(needed as usize, Default::default);
                    count = needed;
                    continue;
                }
                if status == ERROR_SUCCESS {
                    names = infos[..count as usize]
                        .iter()
                        .map(|info| {
                            let len = info.strAppName.iter().position(|&c| c == 0).unwrap_or(0);
                            String::from_utf16_lossy(&info.strAppName[..len])
                        })
                        .collect();
                }
                break;
            }
        }
        RmEndSession(session);
        names.sort();
        names.dedup();
        names
    }
}

fn collect_binaries(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_binaries(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dll") || e.eq_ignore_ascii_case("exe"))
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_test_binary_is_reported_as_in_use() {
        let exe = std::env::current_exe().unwrap();
        let names = programs_using(exe.parent().unwrap());
        assert!(
            !names.is_empty(),
            "expected this test process to hold its own exe"
        );
    }
}
