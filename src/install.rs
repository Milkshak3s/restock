use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256, Sha512};

use crate::catalog::{AppEntry, Installer, expand_env};
use crate::source::Release;
use crate::{elevate, inuse, log};

const ERROR_ELEVATION_REQUIRED: i32 = 740;

/// Downloads into `dir`, verifying the checksum when the source provides one.
pub fn download(
    client: &Client,
    release: &Release,
    dir: &Path,
    progress: impl Fn(String),
) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(&release.file_name);

    let mut resp = client.get(&release.url).send()?.error_for_status()?;
    let total = resp.content_length();
    let mut file = File::create(&path)?;
    let mut sha256 = Sha256::new();
    let mut sha512 = release.sha512.is_some().then(Sha512::new);
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_report = Instant::now();

    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        sha256.update(&buf[..n]);
        if let Some(h) = &mut sha512 {
            h.update(&buf[..n]);
        }
        done += n as u64;
        if last_report.elapsed() > Duration::from_millis(200) {
            last_report = Instant::now();
            progress(match total {
                Some(total) if total > 0 => format!("Downloading… {}%", done * 100 / total),
                _ => format!("Downloading… {:.1} MB", done as f64 / 1_048_576.0),
            });
        }
    }

    if let Some(expected) = &release.sha256 {
        verify("SHA-256", expected, &hex::encode(sha256.finalize()))?;
    }
    if let (Some(expected), Some(h)) = (&release.sha512, sha512) {
        verify("SHA-512", expected, &hex::encode(h.finalize()))?;
    }
    Ok(path)
}

fn verify(kind: &str, expected: &str, actual: &str) -> Result<()> {
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("{kind} mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Runs the installer. `existing` is the folder the app was detected in, if
/// known: zip and portable apps are updated there instead of the catalog's
/// `dest`. When anything fails, the target folder is checked for locked files.
pub fn install(app: &AppEntry, file: &Path, existing: Option<&Path>) -> Result<()> {
    let dest = |configured: &str| {
        existing.map_or_else(|| PathBuf::from(expand_env(configured)), Path::to_path_buf)
    };
    let installer = app.installer.as_ref().context("app has no installer")?;
    let code = match installer {
        Installer::Exe { args } => run(Command::new(file).args(args), false)?,
        // msiexec itself starts fine unelevated, but a silent (/qn) per-machine
        // install then fails instead of prompting, so elevate up front.
        Installer::Msi { args } => run(
            Command::new("msiexec")
                .arg("/i")
                .arg(file)
                .args(["/qn", "/norestart"])
                .args(args),
            !elevate::is_elevated(),
        )?,
        Installer::Zip {
            dest: configured,
            strip_root,
        } => {
            let dest = dest(configured);
            return extract_zip(file, &dest, *strip_root).map_err(|e| explain_locked(e, &dest));
        }
        Installer::Portable {
            dest: configured,
            file_name,
        } => {
            let dest = dest(configured);
            let name = match file_name {
                Some(name) => name.as_ref(),
                None => file.file_name().context("download has no file name")?,
            };
            return fs::create_dir_all(&dest)
                .and_then(|()| fs::copy(file, dest.join(name)))
                .map(drop)
                .map_err(|e| explain_locked(e.into(), &dest));
        }
    };
    match code {
        // 3010 / 1641: success, reboot required / initiated.
        0 | 1641 | 3010 => Ok(()),
        code => Err(installer_failed(app, code, existing)),
    }
}

/// Installs a Microsoft Store product with winget, accepting the Store's and
/// the package's terms. MSIX packages are per-user: an elevated Restock still
/// installs for its own user, which is the signed-in user unless UAC was
/// approved with a different admin account.
pub fn install_from_store(app: &AppEntry, id: &str) -> Result<()> {
    let mut cmd = Command::new("winget");
    cmd.args(["install", "--id", id, "--exact", "--source", "msstore"])
        .args(["--silent", "--disable-interactivity"])
        .args(["--accept-package-agreements", "--accept-source-agreements"])
        .creation_flags(CREATE_NO_WINDOW);
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
    log::write(format!(
        "  run winget {args:?} (restock elevated: {})",
        elevate::is_elevated()
    ));
    let started = Instant::now();
    let output = cmd
        .output()
        .context("could not run winget (is App Installer installed?)")?;
    let text = winget_text(&output.stdout);
    for line in text.lines() {
        log::write(format!("  winget: {line}"));
    }
    let code = output.status.code().context("winget was terminated")?;
    log::write(format!(
        "  exit code {code} (0x{code:08X}) after {:.1}s",
        started.elapsed().as_secs_f64()
    ));
    match code as u32 {
        0 | WINGET_ALREADY_INSTALLED | WINGET_UPDATE_NOT_APPLICABLE | WINGET_REBOOT_TO_FINISH => {
            Ok(())
        }
        code => Err(winget_failed(app, code, &text)),
    }
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const WINGET_ALREADY_INSTALLED: u32 = 0x8A15_0061;
const WINGET_UPDATE_NOT_APPLICABLE: u32 = 0x8A15_002B;
const WINGET_REBOOT_TO_FINISH: u32 = 0x8A15_0109;

/// winget's own descriptions of the failures a Store install is likely to hit
/// (github.com/microsoft/winget-cli, doc/windows/package-manager/winget/returnCodes.md).
const WINGET_ERRORS: &[(u32, &str)] = &[
    (0x8A15_0005, "cancelled"),
    (0x8A15_0008, "download failed"),
    (
        0x8A15_0014,
        "no package with that id in the Microsoft Store",
    ),
    (
        0x8A15_001B,
        "the Microsoft Store client is blocked by policy",
    ),
    (0x8A15_001C, "the Microsoft Store app is blocked by policy"),
    (0x8A15_001E, "the Microsoft Store install failed"),
    (0x8A15_0041, "package agreements were not accepted"),
    (0x8A15_0046, "source agreements were not accepted"),
    (
        0x8A15_007D,
        "not permitted from an administrator context for per-user packages",
    ),
    (0x8A15_007F, "could not reach the Microsoft Store catalog"),
    (
        0x8A15_0083,
        "could not get a Microsoft Store license (sign in to the Store app)",
    ),
    (0x8A15_0101, "the app is running; close it and retry"),
    (0x8A15_0102, "another installation is in progress"),
    (0x8A15_0104, "a dependency is missing"),
    (0x8A15_0105, "the disk is full"),
    (0x8A15_0107, "no network connection"),
    (0x8A15_010A, "restart the PC, then retry"),
    (0x8A15_010C, "cancelled"),
    (0x8A15_010F, "blocked by organization policy"),
    (0x8A15_0110, "installing dependencies failed"),
    (0x8A15_0113, "not supported on this system"),
];

/// Explains a failed winget exit code, with winget's last line of output.
fn winget_failed(app: &AppEntry, code: u32, output: &str) -> anyhow::Error {
    let mut msg = format!("winget exited with 0x{code:08X}");
    let meaning = app
        .exit_codes
        .get(&(code as i32).to_string())
        .map(String::as_str);
    let known = WINGET_ERRORS
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, m)| *m);
    if let Some(meaning) = meaning.or(known) {
        msg = format!("{msg}: {meaning}");
    }
    if let Some(last) = output.lines().last() {
        msg = format!("{msg} (winget: {last})");
    }
    anyhow::anyhow!(msg)
}

/// winget's console output without progress bars and spinners: the last
/// redraw of each line, trimmed, non-empty lines only.
fn winget_text(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.rsplit('\r').map(str::trim).find(|s| !s.is_empty()))
        .filter(|line| !line.contains(['█', '▒']) && !matches!(*line, "-" | "\\" | "|" | "/"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Explains a failed exit code using the catalog's `exit_codes`, and names
/// any programs holding files in the install folder open.
fn installer_failed(app: &AppEntry, code: i32, install_dir: Option<&Path>) -> anyhow::Error {
    let mut msg = format!("installer exited with code {code}");
    if let Some(meaning) = app.exit_codes.get(&code.to_string()) {
        msg = format!("{msg}: {meaning}");
    }
    if let Some(note) = install_dir.and_then(locked_by) {
        msg = format!("{msg}. {note}");
    }
    anyhow::anyhow!(msg)
}

/// Adds which programs hold files in `dir` open to a failed zip/portable
/// update, e.g. a tray app whose exe can't be overwritten while it runs.
fn explain_locked(err: anyhow::Error, dir: &Path) -> anyhow::Error {
    match locked_by(dir) {
        Some(note) => err.context(note),
        None => err,
    }
}

fn locked_by(dir: &Path) -> Option<String> {
    let programs = inuse::programs_using(dir);
    log::write(format!("  programs using {}: {programs:?}", dir.display()));
    (!programs.is_empty()).then(|| {
        format!(
            "Files in {} are in use by: {}. Close them and retry",
            dir.display(),
            programs.join(", ")
        )
    })
}

/// Runs the installer and returns its exit code. Installers whose manifest
/// requires admin can't be started from an unelevated process (error 740), so
/// those are relaunched through a UAC prompt, as is anything `elevate` asks for.
fn run(cmd: &mut Command, elevate: bool) -> Result<i32> {
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
    log::write(format!(
        "  run {:?} {args:?} (restock elevated: {}, via UAC: {elevate})",
        cmd.get_program(),
        elevate::is_elevated(),
    ));
    let started = Instant::now();
    let code = if elevate {
        elevate::run_elevated(cmd.get_program(), cmd.get_args())?
    } else {
        match cmd.status() {
            Ok(status) => status.code().context("installer was terminated")?,
            Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
                log::write("  error 740 (elevation required), retrying via UAC");
                elevate::run_elevated(cmd.get_program(), cmd.get_args())?
            }
            Err(e) => return Err(e.into()),
        }
    };
    log::write(format!(
        "  exit code {code} (0x{code:08X}) after {:.1}s",
        started.elapsed().as_secs_f64()
    ));
    Ok(code)
}

fn extract_zip(file: &Path, dest: &Path, strip_root: bool) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(file)?)?;
    fs::create_dir_all(dest)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let rel: PathBuf = if strip_root {
            rel.components().skip(1).collect()
        } else {
            rel
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            io::copy(&mut entry, &mut File::create(&out)?)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    #[test]
    fn zip_update_goes_into_detected_folder_and_keeps_other_files() {
        let root = std::env::temp_dir().join(format!("restock-test-{}", std::process::id()));
        let existing = root
            .join("Documents")
            .join("EVEAPMPreview-v0.9.1")
            .join("EVEAPMPreview");
        fs::create_dir_all(existing.join("profiles")).unwrap();
        fs::write(existing.join("EVEAPMPreview.exe"), "old").unwrap();
        fs::write(existing.join("profiles").join("primary.ini"), "mine").unwrap();

        // Same shape as the release zip: a single root folder holding the exe.
        let archive = root.join("EVEAPMPreview-v0.9.9.zip");
        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("EVEAPMPreview/EVEAPMPreview.exe", options)
            .unwrap();
        zip.write_all(b"new").unwrap();
        zip.finish().unwrap();

        let app = app_with(Installer::Zip {
            dest: root.join("fresh").display().to_string(),
            strip_root: true,
        });
        install(&app, &archive, Some(&existing)).unwrap();

        let read = |p: PathBuf| fs::read_to_string(p).unwrap();
        assert_eq!(read(existing.join("EVEAPMPreview.exe")), "new");
        assert_eq!(read(existing.join("profiles").join("primary.ini")), "mine");
        assert!(!root.join("fresh").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn failure_message_uses_catalog_exit_codes() {
        let mut app = app_with(Installer::Exe { args: vec![] });
        app.exit_codes
            .insert("6".into(), "OBS files are in use".into());
        let known = installer_failed(&app, 6, None).to_string();
        assert_eq!(known, "installer exited with code 6: OBS files are in use");
        let unknown = installer_failed(&app, 2, None).to_string();
        assert_eq!(unknown, "installer exited with code 2");
    }

    #[test]
    fn winget_output_drops_progress_and_explains_codes() {
        let stdout = concat!(
            "   - \r   \\ \r\r\n",
            "Found Apple Devices [9NP83LWLPZ9K] Version Unknown\r\n",
            "This package is provided through Microsoft Store.\r\n",
            "\r  ██████████▒▒▒▒  1.00 MB / 2.00 MB\r  ████████████  2.00 MB / 2.00 MB\r\n",
            "Failed when searching source: msstore\r\n",
        );
        let text = winget_text(stdout.as_bytes());
        assert_eq!(
            text,
            "Found Apple Devices [9NP83LWLPZ9K] Version Unknown\n\
             This package is provided through Microsoft Store.\n\
             Failed when searching source: msstore"
        );
        let app = app_with(Installer::Exe { args: vec![] });
        assert_eq!(
            winget_failed(&app, 0x8A15_0014, &text).to_string(),
            "winget exited with 0x8A150014: no package with that id in the Microsoft Store \
             (winget: Failed when searching source: msstore)"
        );
        assert_eq!(
            winget_failed(&app, 0x8A15_FFFF, "").to_string(),
            "winget exited with 0x8A15FFFF"
        );
    }

    #[test]
    fn locked_folder_errors_name_the_program() {
        // This test's own exe is running, so its folder counts as locked.
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let text = format!(
            "{:#}",
            explain_locked(anyhow::anyhow!("Access is denied"), &dir)
        );
        assert!(text.starts_with("Files in "), "{text}");
        assert!(
            text.ends_with("Close them and retry: Access is denied"),
            "{text}"
        );
    }

    fn app_with(installer: Installer) -> AppEntry {
        AppEntry {
            id: "test".into(),
            name: "Test".into(),
            description: String::new(),
            essential: false,
            self_updating: false,
            exit_codes: Default::default(),
            source: crate::catalog::Source::Direct {
                url: "https://example.com/test.zip".into(),
                version: "1.0".into(),
                sha256: None,
            },
            installer: Some(installer),
            detect: crate::catalog::Detect::Managed,
        }
    }
}
