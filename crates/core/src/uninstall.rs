//! Remove the Affinity prefix, desktop entries and URL handler.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::journal::Report;
use crate::runner::{self, RunOptions, Stream};
use crate::{WINE_DIR_NAME, config};

#[derive(Debug, Clone)]
pub struct UninstallOptions {
    pub prefix: PathBuf,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct UninstallOutcome {
    pub removed_entries: usize,
    pub prefix_removed: bool,
}

pub fn uninstall<F>(opts: &UninstallOptions, on_line: F) -> Result<UninstallOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let mut outcome = UninstallOutcome::default();

    report.info(format!(
        "Uninstalling Affinity from {}",
        opts.prefix.display()
    ));

    if opts.dry_run {
        for path in desktop_files() {
            report.info(format!("would remove {}", path.display()));
        }
        if opts.prefix.exists() {
            report.info(format!("would run rm -rf {}", opts.prefix.display()));
        } else {
            report.info(format!("{} does not exist", opts.prefix.display()));
        }
        return Ok(outcome);
    }

    stop_prefix(&opts.prefix, &mut report);

    for path in desktop_files() {
        if path.exists() {
            match fs::remove_file(&path) {
                Ok(()) => {
                    outcome.removed_entries += 1;
                    report.info(format!("removed {}", path.display()));
                }
                Err(error) => report.warn(format!("could not remove {}: {error}", path.display())),
            }
        }
    }
    if outcome.removed_entries > 0 {
        refresh_desktop_database(&mut report);
    }

    if opts.prefix.exists() {
        report.info(format!("deleting {}", opts.prefix.display()));
        if let Err(error) = fs::remove_dir_all(&opts.prefix) {
            report.warn(format!(
                "initial removal failed ({error}); retrying after chmod"
            ));
            make_writable(&opts.prefix);
            if fs::remove_dir_all(&opts.prefix).is_err() {
                report.warn("retry failed; falling back to rm -rf");
                let _ = Command::new("rm")
                    .arg("-rf")
                    .arg(&opts.prefix)
                    .status()
                    .context("running rm -rf")?;
            }
        }
        if opts.prefix.exists() {
            bail!(
                "could not delete {}; close Affinity and any Wine processes, then retry",
                opts.prefix.display()
            );
        }
        outcome.prefix_removed = true;
        report.info("prefix deleted");
    } else {
        report.info("prefix not found; nothing to delete");
    }

    let reset = config::default_prefix();
    report.info(format!("install location reset to {}", reset.display()));
    Ok(outcome)
}

fn stop_prefix(prefix: &Path, report: &mut Report<'_>) {
    let wine_dir = prefix.join(WINE_DIR_NAME);
    let wineserver = if wine_dir.join("bin/wineserver").exists() {
        wine_dir.join("bin/wineserver")
    } else {
        PathBuf::from("wineserver")
    };
    if wineserver == Path::new("wineserver") && crate::deps::which("wineserver").is_none() {
        return;
    }
    let mut cmd = Command::new(&wineserver);
    cmd.arg("-k");
    cmd.env("WINEPREFIX", prefix);
    match runner::run(
        cmd,
        RunOptions::default().stall(Duration::from_secs(30)).quiet(),
        |stream, line| report.line(stream, line),
    ) {
        Ok(_) => std::thread::sleep(Duration::from_millis(800)),
        Err(error) => report.warn(format!("could not stop Wine processes: {error}")),
    }
}

fn refresh_desktop_database(report: &mut Report<'_>) {
    if crate::deps::which("update-desktop-database").is_none() {
        return;
    }
    if let Some(dir) = applications_dir() {
        let mut cmd = Command::new("update-desktop-database");
        cmd.arg(dir);
        let _ = runner::run(
            cmd,
            RunOptions::default().stall(Duration::from_secs(60)).quiet(),
            |stream, line| report.line(stream, line),
        );
    }
}

fn make_writable(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o777));
    if metadata.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            make_writable(&entry.path());
        }
    }
}

pub fn applications_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(base.join("applications"))
}

fn desktop_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Some(apps) = applications_dir() else {
        return files;
    };
    for name in [
        "Affinity.desktop",
        "AffinityPhoto.desktop",
        "AffinityDesigner.desktop",
        "AffinityPublisher.desktop",
        "affinity-url-handler.desktop",
        "wine-protocol-affinity.desktop",
    ] {
        files.push(apps.join(name));
    }
    for name in [
        "Affinity.desktop",
        "Affinity Photo 2.desktop",
        "Affinity Photo.desktop",
        "Affinity Designer 2.desktop",
        "Affinity Designer.desktop",
        "Affinity Publisher 2.desktop",
        "Affinity Publisher.desktop",
    ] {
        files.push(apps.join("wine/Programs").join(name));
    }
    if let Some(home) = std::env::var_os("HOME") {
        files.push(PathBuf::from(home).join("Desktop/Affinity.desktop"));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_keeps_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        fs::write(prefix.join("marker"), b"keep").unwrap();
        let opts = UninstallOptions {
            prefix: prefix.clone(),
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        let outcome = uninstall(&opts, |_, line| lines.push(line.to_string())).unwrap();
        assert!(!outcome.prefix_removed);
        assert!(prefix.join("marker").exists());
        let text = lines.join("\n");
        assert!(text.contains("would run rm -rf"));
        assert!(text.contains("Affinity.desktop"));
    }

    #[test]
    fn applications_dir_stays_under_xdg_or_home() {
        let dir = applications_dir().expect("applications dir");
        assert!(dir.ends_with("applications"));
    }
}
