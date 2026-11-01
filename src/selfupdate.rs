//! Restock updating itself. The installed exe can't be replaced while it runs, so Restock hands
//! the new MSI to a detached msiexec, closes, and the new version is started once it's installed.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Result;

/// Restock's own id in `catalog.toml`.
pub const APP_ID: &str = "restock";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether this process is the copy installed in `dir` (the one its MSI replaces), rather than a
/// development build or a portable exe, which update like any other app.
pub fn is_running_from(dir: &Path) -> bool {
    let running = std::env::current_exe()
        .and_then(fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    running.is_some() && running == fs::canonicalize(dir).ok()
}

/// Starts a hidden `cmd` that waits for Restock to exit, upgrades it from `msi` with a progress
/// bar and no questions, and starts `restock.exe` from `dir` again (the old one if it failed).
/// The caller must close Restock right after.
pub fn hand_off(msi: &Path, dir: &Path) -> Result<()> {
    let exe = dir.join("restock.exe");
    // `ping` waits without needing a console, unlike `timeout`. `/s` strips just the outer quotes.
    let line = format!(
        r#"/s /c "ping -n 4 127.0.0.1 >nul & msiexec /i "{}" /passive /norestart & start "" "{}"""#,
        msi.display(),
        exe.display()
    );
    Command::new("cmd")
        .raw_arg(line)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_its_own_folder_only() {
        let exe = std::env::current_exe().unwrap();
        assert!(is_running_from(exe.parent().unwrap()));
        assert!(!is_running_from(&std::env::temp_dir()));
        assert!(!is_running_from(Path::new(r"C:\definitely\not\here")));
    }

    #[test]
    fn catalog_lists_restock_as_an_msi() {
        let catalog = crate::catalog::load().unwrap();
        let me = catalog
            .apps
            .iter()
            .find(|a| a.id == APP_ID)
            .expect("restock in catalog");
        assert!(matches!(
            me.installer,
            Some(crate::catalog::Installer::Msi { .. })
        ));
    }
}
