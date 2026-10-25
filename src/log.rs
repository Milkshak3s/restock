//! Append-only install log at %LOCALAPPDATA%\restock\restock.log, so failed
//! installs can be diagnosed after the fact.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

pub fn path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("restock")
        .join("restock.log")
}

pub fn write(line: impl AsRef<str>) {
    if cfg!(test) {
        return;
    }
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let now = humantime::format_rfc3339_seconds(SystemTime::now());
        let _ = writeln!(file, "{now} {}", line.as_ref());
    }
}
