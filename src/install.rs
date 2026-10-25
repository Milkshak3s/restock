use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

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
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_report = Instant::now();

    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
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
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            bail!("checksum mismatch: expected {expected}, got {actual}");
        }
    }
    Ok(path)
}

/// Runs the installer. `existing` is the folder the app was detected in, if
/// known: zip and portable apps are updated there instead of the catalog's
/// `dest`, and for installers it's checked for locked files when they fail.
pub fn install(app: &AppEntry, file: &Path, existing: Option<&Path>) -> Result<()> {
    let dest = |configured: &str| {
        existing.map_or_else(|| PathBuf::from(expand_env(configured)), Path::to_path_buf)
    };
    let code = match &app.installer {
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
        } => return extract_zip(file, &dest(configured), *strip_root),
        Installer::Portable {
            dest: configured,
            file_name,
        } => {
            let dest = dest(configured);
            fs::create_dir_all(&dest)?;
            let name = match file_name {
                Some(name) => name.as_ref(),
                None => file.file_name().context("download has no file name")?,
            };
            fs::copy(file, dest.join(name))?;
            return Ok(());
        }
    };
    match code {
        // 3010 / 1641: success, reboot required / initiated.
        0 | 1641 | 3010 => Ok(()),
        code => Err(installer_failed(app, code, existing)),
    }
}

/// Explains a failed exit code using the catalog's `exit_codes`, and names
/// any programs holding files in the install folder open.
fn installer_failed(app: &AppEntry, code: i32, install_dir: Option<&Path>) -> anyhow::Error {
    let mut msg = format!("installer exited with code {code}");
    if let Some(meaning) = app.exit_codes.get(&code.to_string()) {
        msg = format!("{msg}: {meaning}");
    }
    if let Some(dir) = install_dir {
        let programs = inuse::programs_using(dir);
        log::write(format!("  programs using {}: {programs:?}", dir.display()));
        if !programs.is_empty() {
            msg = format!(
                "{msg}. Files in {} are in use by: {}. Close them and retry",
                dir.display(),
                programs.join(", ")
            );
        }
    }
    anyhow::anyhow!(msg)
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
            installer,
            detect: crate::catalog::Detect::Managed,
        }
    }
}
