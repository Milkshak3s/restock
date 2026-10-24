use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

use crate::catalog::{Installer, expand_env};
use crate::source::Release;

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

/// Runs the installer. Zip and portable apps go into `existing` (the folder
/// they were detected in) when known, otherwise into the catalog's `dest`.
pub fn install(installer: &Installer, file: &Path, existing: Option<&Path>) -> Result<()> {
    let dest = |configured: &str| {
        existing.map_or_else(|| PathBuf::from(expand_env(configured)), Path::to_path_buf)
    };
    match installer {
        Installer::Exe { args } => run(Command::new(file).args(args)),
        Installer::Msi { args } => run(Command::new("msiexec")
            .arg("/i")
            .arg(file)
            .args(["/qn", "/norestart"])
            .args(args)),
        Installer::Zip {
            dest: configured,
            strip_root,
        } => extract_zip(file, &dest(configured), *strip_root),
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
            Ok(())
        }
    }
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().map_err(|e| {
        if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) {
            anyhow!("installer needs administrator rights; run Restock as administrator")
        } else {
            e.into()
        }
    })?;
    match status.code() {
        // 3010 / 1641: success, reboot required / initiated.
        Some(0 | 1641 | 3010) => Ok(()),
        Some(code) => bail!("installer exited with code {code}"),
        None => bail!("installer was terminated"),
    }
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

        let installer = Installer::Zip {
            dest: root.join("fresh").display().to_string(),
            strip_root: true,
        };
        install(&installer, &archive, Some(&existing)).unwrap();

        let read = |p: PathBuf| fs::read_to_string(p).unwrap();
        assert_eq!(read(existing.join("EVEAPMPreview.exe")), "new");
        assert_eq!(read(existing.join("profiles").join("primary.ini")), "mine");
        assert!(!root.join("fresh").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
