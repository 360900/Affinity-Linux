//! Settings-save fix: stage the AffinityPatcher sources and run the IL patch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::deps::which;
use crate::http;
use crate::journal::Report;
use crate::runner::{self, RunOptions, Stream};
use crate::seven_zip_out;

const PATCHER_CS: &str = "https://raw.githubusercontent.com/ryzendew/AffinityOnLinux/main/Patch/AffinityPatcherSettings/AffinityPatcher.cs";
const PATCHER_CSPROJ: &str = "https://raw.githubusercontent.com/ryzendew/AffinityOnLinux/main/Patch/AffinityPatcherSettings/AffinityPatcher.csproj";
const COLORS_REPO: &str = "https://github.com/ShawnTheBeachy/return-affinity-colors.git";
const COLORS_ZIP: &str =
    "https://github.com/ShawnTheBeachy/return-affinity-colors/archive/refs/heads/main.zip";

#[derive(Debug, Clone, Default)]
pub struct PatchOutcome {
    pub patched: bool,
    pub skipped: Option<String>,
}

pub fn apply<F>(
    prefix: &Path,
    dry_run: bool,
    log_path: Option<PathBuf>,
    on_line: F,
) -> Result<PatchOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);

    report.info("Affinity settings patch");
    stage_patcher(prefix, dry_run, &mut report)?;
    ensure_return_colors(prefix, dry_run, &mut report)?;

    if dry_run {
        report.info(format!(
            "    dotnet build {} -c Release -o {} --no-incremental -p:BuildProjectReferences=false",
            prefix
                .join("Patch/AffinityPatcherSettings/AffinityPatcher.csproj")
                .display(),
            prefix
                .join("Patch/AffinityPatcherSettings/bin/Release")
                .display()
        ));
        report.info(format!(
            "    dotnet {} {}",
            prefix
                .join("Patch/AffinityPatcherSettings/bin/Release/AffinityPatcher.dll")
                .display(),
            dll_path(prefix).display()
        ));
        return Ok(PatchOutcome::default());
    }

    let dll = dll_path(prefix);
    if !dll.exists() {
        let reason = format!("{} is missing", dll.display());
        report.warn(format!("skipping the settings patch: {reason}"));
        return Ok(PatchOutcome {
            patched: false,
            skipped: Some(reason),
        });
    }

    if !has_dotnet_sdk() {
        let reason = ".NET SDK 8 or newer is missing".to_string();
        report.warn(format!(
            "skipping the settings patch: {reason}; install dotnet-sdk to enable it"
        ));
        return Ok(PatchOutcome {
            patched: false,
            skipped: Some(reason),
        });
    }

    let patcher = build_patcher(prefix, &mut report)?;
    backup_dll(&dll, &mut report)?;

    let cmd = if patcher.extension().and_then(|ext| ext.to_str()) == Some("dll") {
        let mut cmd = Command::new("dotnet");
        cmd.arg(&patcher).arg(&dll);
        cmd
    } else {
        let mut cmd = Command::new(&patcher);
        cmd.arg(&dll);
        cmd
    };
    let result = runner::run(
        cmd,
        RunOptions::default().stall(Duration::from_secs(300)),
        |stream, line| report.line(stream, line),
    )?;
    if !result.success() {
        report.warn(format!(
            "AffinityPatcher failed with {}; settings saving may not work",
            result.status
        ));
        return Ok(PatchOutcome {
            patched: false,
            skipped: Some("AffinityPatcher failed".to_string()),
        });
    }

    report.info("Serif.Affinity.dll patched for settings saving");
    Ok(PatchOutcome {
        patched: true,
        skipped: None,
    })
}

fn stage_patcher(prefix: &Path, dry_run: bool, report: &mut Report<'_>) -> Result<()> {
    let directory = prefix.join("Patch/AffinityPatcherSettings");
    let files = [
        ("AffinityPatcher.cs", PATCHER_CS),
        ("AffinityPatcher.csproj", PATCHER_CSPROJ),
    ];

    for (name, url) in files {
        let destination = directory.join(name);
        if destination.exists() {
            continue;
        }
        if let Some(local) = local_source(name) {
            report.info(format!("copying {name} from {}", local.display()));
            if dry_run {
                report.info(format!(
                    "    cp {} {}",
                    local.display(),
                    destination.display()
                ));
            } else {
                fs::create_dir_all(&directory)?;
                fs::copy(&local, &destination).with_context(|| {
                    format!("copying {} to {}", local.display(), destination.display())
                })?;
            }
            continue;
        }

        report.info(format!("downloading {name}"));
        if dry_run {
            report.info(format!(
                "    curl -fL --create-dirs -o {} {url}",
                destination.display()
            ));
        } else {
            http::download(report, url, &destination, Duration::from_secs(120))?;
        }
    }
    Ok(())
}

fn local_source(name: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    [
        cwd.join("Patch/AffinityPatcherSettings").join(name),
        cwd.join("../Patch/AffinityPatcherSettings").join(name),
    ]
    .into_iter()
    .find(|candidate| candidate.exists())
}

fn ensure_return_colors(prefix: &Path, dry_run: bool, report: &mut Report<'_>) -> Result<()> {
    let directory = prefix.join("Patch/return-affinity-colors");
    if directory.join("ReturnColors/ReturnColors.csproj").exists() {
        return Ok(());
    }

    if dry_run {
        report.info(format!(
            "    git clone --depth 1 {COLORS_REPO} {}",
            directory.display()
        ));
        return Ok(());
    }

    fs::create_dir_all(prefix.join("Patch"))?;
    let cloned = if which("git").is_some() {
        let mut cmd = Command::new("git");
        cmd.args(["clone", "--depth", "1", COLORS_REPO])
            .arg(&directory);
        let result = runner::run(
            cmd,
            RunOptions::default().stall(Duration::from_secs(300)),
            |stream, line| report.line(stream, line),
        )?;
        result.success()
    } else {
        false
    };

    if cloned {
        return Ok(());
    }

    report.warn("git clone failed; downloading ReturnColors as a zip");
    if which("7z").is_none() {
        report.warn("7z is missing, skipping ReturnColors");
        return Ok(());
    }
    let zip = prefix.join("Patch/return-affinity-colors.zip");
    http::download(report, COLORS_ZIP, &zip, Duration::from_secs(300))?;
    let temp = prefix.join("Patch/.return-colors-tmp");
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    let mut cmd = Command::new("7z");
    cmd.args(["x", "-y"]).arg(seven_zip_out(&temp)).arg(&zip);
    let _ = runner::run(
        cmd,
        RunOptions::default().stall(Duration::from_secs(300)),
        |stream, line| report.line(stream, line),
    )?;
    if let Some(extracted) = fs::read_dir(&temp)?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && path.join("ReturnColors/ReturnColors.csproj").exists())
    {
        let _ = fs::remove_dir_all(&directory);
        fs::rename(extracted, &directory)?;
    }
    let _ = fs::remove_dir_all(&temp);
    let _ = fs::remove_file(&zip);
    Ok(())
}

fn has_dotnet_sdk() -> bool {
    which("dotnet").is_some()
        && Command::new("dotnet")
            .args(["--list-sdks"])
            .output()
            .map(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
            .unwrap_or(false)
}

fn build_patcher(prefix: &Path, report: &mut Report<'_>) -> Result<PathBuf> {
    let directory = prefix.join("Patch/AffinityPatcherSettings");
    let project = directory.join("AffinityPatcher.csproj");
    let output = directory.join("bin/Release");

    let mut cmd = Command::new("dotnet");
    cmd.arg("build")
        .arg(&project)
        .args(["-c", "Release", "-o"])
        .arg(&output)
        .args([
            "--no-incremental",
            // Otherwise the compiler server keeps running after the install.
            "--disable-build-servers",
            "-p:BuildProjectReferences=false",
            "/p:DisableImplicitNuGetFallbackFolder=true",
        ]);
    let result = runner::run(
        cmd,
        RunOptions::default().stall(Duration::from_secs(600)),
        |stream, line| report.line(stream, line),
    )?;
    if !result.success() {
        anyhow::bail!(
            "dotnet build failed for {}\n{}",
            project.display(),
            result.tail_text(20)
        );
    }

    for name in [
        "AffinityPatcher",
        "AffinityPatcher.dll",
        "AffinityPatcher.exe",
    ] {
        let candidate = output.join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "built AffinityPatcher was not found in {}",
        output.display()
    )
}

fn backup_dll(dll: &Path, report: &mut Report<'_>) -> Result<()> {
    let backup = dll.with_extension("dll.bak");
    if backup.exists() {
        return Ok(());
    }
    fs::copy(dll, &backup)
        .with_context(|| format!("backing up {} to {}", dll.display(), backup.display()))?;
    report.info(format!("backed up the DLL to {}", backup.display()));
    Ok(())
}

fn dll_path(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/Program Files/Affinity/Affinity/Serif.Affinity.dll")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_has_no_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = Vec::new();
        let outcome = apply(dir.path(), true, None, |_, line| {
            lines.push(line.to_string())
        })
        .unwrap();
        let text = lines.join("\n");
        assert!(text.contains("AffinityPatcher.cs"));
        assert!(text.contains("git clone"));
        assert!(text.contains("dotnet build"));
        assert!(!outcome.patched);
        assert!(!dir.path().join("Patch").exists());
    }

    #[test]
    fn dll_lives_in_the_app_directory() {
        assert!(
            dll_path(Path::new("/p"))
                .ends_with("drive_c/Program Files/Affinity/Affinity/Serif.Affinity.dll")
        );
    }
}
