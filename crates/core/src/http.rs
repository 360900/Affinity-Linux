//! Small network helpers built on curl/wget so the core stays dependency-free.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::deps::which;
use crate::journal::Report;
use crate::runner::{self, RunOptions};

const USER_AGENT: &str = "affinity-linux";
const CURL_DOWNLOAD: &[&str] = &[
    "-fL",
    "--retry",
    "3",
    "--retry-delay",
    "2",
    "--connect-timeout",
    "30",
    "--progress-bar",
    "-A",
    USER_AGENT,
];

/// `$XDG_CACHE_HOME`, else `~/.cache`.
pub(crate) fn xdg_cache_home() -> Result<PathBuf> {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(cache));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".cache"))
}

/// Large downloads kept outside the prefix, so a reinstall or a new prefix
/// does not fetch them again.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(xdg_cache_home()?.join(crate::APP_NAME))
}

fn file_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(path.to_path_buf()).or_default().clone()
}

/// Download `url` into the cache as `name`. A cached copy is revalidated with
/// curl's `-z` against the server's Last-Modified, so an unchanged file costs
/// one request and a newer release is still picked up. When the server cannot
/// be reached the cached copy is used.
pub(crate) fn download_cached(
    report: &mut Report<'_>,
    url: &str,
    name: &str,
    stall: Duration,
) -> Result<PathBuf> {
    let path = cache_dir()?.join(name);
    // A background prefetch may be fetching the same file.
    let lock = file_lock(&path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !path.is_file() {
        download(report, url, &path, stall)?;
        return Ok(path);
    }
    if which("curl").is_none() {
        report.info(format!("using cached {}", path.display()));
        return Ok(path);
    }

    let partial = path.with_extension("check.part");
    let _ = std::fs::remove_file(&partial);
    report.info(format!("checking {url} for a newer {name}"));
    let mut cmd = Command::new("curl");
    cmd.args(CURL_DOWNLOAD)
        .arg("-R")
        .arg("-z")
        .arg(&path)
        .arg("-o")
        .arg(&partial)
        .arg(url);
    match run(report, cmd, stall) {
        // On 304 Not Modified curl writes no file.
        Ok(true)
            if partial
                .metadata()
                .map(|meta| meta.len() > 0)
                .unwrap_or(false) =>
        {
            finish(partial, &path)?;
            report.info(format!("downloaded a newer {name}"));
        }
        Ok(true) => {
            let _ = std::fs::remove_file(&partial);
            report.info(format!("cached {name} is current"));
        }
        _ => {
            let _ = std::fs::remove_file(&partial);
            report.warn(format!(
                "could not check for a newer {name}; using the cached copy"
            ));
        }
    }
    Ok(path)
}

/// Fetch a small text document (JSON API, key file) into memory.
pub(crate) fn fetch_text(url: &str) -> Result<String> {
    let output = if which("curl").is_some() {
        Command::new("curl")
            .args([
                "-fsSL",
                "--retry",
                "3",
                "--retry-delay",
                "2",
                "--connect-timeout",
                "20",
                "-A",
                USER_AGENT,
                url,
            ])
            .output()
            .with_context(|| format!("running curl for {url}"))?
    } else if which("wget").is_some() {
        Command::new("wget")
            .args(["-qO-", "--tries=3", "--timeout=20", "-U", USER_AGENT, url])
            .output()
            .with_context(|| format!("running wget for {url}"))?
    } else {
        bail!("curl or wget is required to download {url}");
    };

    if !output.status.success() {
        bail!(
            "failed to fetch {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).with_context(|| format!("decoding {url}"))
}

/// Download `url` into `destination`, removing a partial file on failure.
pub(crate) fn download(
    report: &mut Report<'_>,
    url: &str,
    destination: &Path,
    stall: Duration,
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let partial = destination.with_extension(format!(
        "{}.part",
        destination
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("download")
    ));
    report.info(format!("downloading {url}"));

    let mut last_error = String::new();
    if which("curl").is_some() {
        let mut cmd = Command::new("curl");
        cmd.args(CURL_DOWNLOAD)
            // Keep the server's timestamp so `download_cached` can revalidate.
            .arg("-R")
            .arg("-o")
            .arg(&partial)
            .arg(url);
        match run(report, cmd, stall) {
            Ok(true) => return finish(partial, destination),
            Ok(false) => last_error = "curl failed".to_string(),
            Err(error) => last_error = error.to_string(),
        }
    }

    if which("wget").is_some() {
        let mut cmd = Command::new("wget");
        cmd.args(["--tries=3", "--timeout=30", "-U", USER_AGENT, "-O"])
            .arg(&partial)
            .arg(url);
        match run(report, cmd, stall) {
            Ok(true) => return finish(partial, destination),
            Ok(false) => last_error = format!("{last_error}; wget failed"),
            Err(error) => last_error = format!("{last_error}; {error}"),
        }
    }

    let _ = std::fs::remove_file(&partial);
    bail!("could not download {url}: {last_error}");
}

fn run(report: &mut Report<'_>, cmd: Command, stall: Duration) -> Result<bool> {
    let result = runner::run(cmd, RunOptions::default().stall(stall), |stream, line| {
        report.line(stream, line)
    })?;
    Ok(result.success())
}

fn finish(partial: std::path::PathBuf, destination: &Path) -> Result<()> {
    std::fs::rename(&partial, destination)
        .with_context(|| format!("moving {} to {}", partial.display(), destination.display()))
}

/// Hex SHA-256 of a file, using the system `sha256sum`.
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    if which("sha256sum").is_none() {
        bail!("sha256sum is required to verify downloads");
    }
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| format!("hashing {}", path.display()))?;
    if !output.status.success() {
        bail!("sha256sum failed for {}", path.display());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let hash = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if hash.len() != 64 {
        bail!("unexpected sha256sum output for {}", path.display());
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_lowercase_hex() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data");
        std::fs::write(&file, b"affinity").unwrap();
        let hash = sha256_file(&file).unwrap();
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
