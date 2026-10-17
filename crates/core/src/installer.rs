//! Run the Affinity v3 setup executable, with the embedded-MSI fallback.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::deps::which;
use crate::http;
use crate::journal::Report;
use crate::manifest::now_epoch;
use crate::runner::{self, RunOptions, RunResult, Stream};
use crate::{WINE_DIR_NAME, app_exe};

pub const INSTALLER_URL: &str = "https://downloads.affinity.studio/Affinity%20x64.exe";
pub const INSTALLER_NAME: &str = "Affinity-x64.exe";

#[derive(Debug, Clone)]
pub struct InstallerOptions {
    pub prefix: PathBuf,
    /// Optional user-supplied setup executable.
    pub installer: Option<PathBuf>,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct InstallerOutcome {
    pub installer: PathBuf,
    pub used_msi_fallback: bool,
}

pub fn install<F>(opts: &InstallerOptions, on_line: F) -> Result<InstallerOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let wine_dir = opts.prefix.join(WINE_DIR_NAME);
    let wine = wine_dir.join("bin/wine");

    report.info(format!("Affinity setup for {}", opts.prefix.display()));
    if !opts.dry_run && !wine.exists() {
        bail!(
            "Wine is not installed at {}; run wine-setup first",
            wine.display()
        );
    }

    let installer = resolve_installer(opts, &mut report)?;

    if opts.dry_run {
        dry_run(opts, &installer, &wine, &wine_dir, &mut report);
        return Ok(InstallerOutcome {
            installer,
            used_msi_fallback: false,
        });
    }

    let envs = wine_env(&opts.prefix, &wine_dir);
    let _ = kill_prefix(&wine_dir, &envs, &mut report);

    let mut winecfg = Command::new(wine_dir.join("bin/winecfg"));
    winecfg.args(["-v", "win11"]);
    if let Err(error) = run(&mut report, winecfg, &envs, Duration::from_secs(120)) {
        report.warn(format!("could not set Windows 11: {error}"));
    }

    report.info(format!("running {}", installer.display()));
    let mut cmd = Command::new(&wine);
    cmd.arg(&installer);
    let result = run(&mut report, cmd, &envs, Duration::from_secs(3600))?;
    if !result.success() {
        report.warn(format!(
            "the setup exited with {}; checking whether Affinity was installed anyway",
            result.status
        ));
    }

    let mut used_msi_fallback = false;
    if !app_exe(&opts.prefix).exists() {
        report.warn("Affinity.exe was not installed by the setup; trying the embedded MSI");
        msi_fallback(&opts.prefix, &wine_dir, &wine, &installer, &mut report)?;
        used_msi_fallback = true;
    }

    if !app_exe(&opts.prefix).exists() {
        bail!(
            "Affinity.exe was not created; the MSI log is {}",
            opts.prefix.join("affinity-msi.log").display()
        );
    }

    report.info("Affinity v3 installed");
    Ok(InstallerOutcome {
        installer,
        used_msi_fallback,
    })
}

fn resolve_installer(opts: &InstallerOptions, report: &mut Report<'_>) -> Result<PathBuf> {
    let directory = opts.prefix.join("Installer");
    let destination = match &opts.installer {
        Some(source) => directory.join(
            source
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new(INSTALLER_NAME)),
        ),
        None => directory.join(INSTALLER_NAME),
    };

    if opts.dry_run {
        if let Some(source) = &opts.installer {
            report.info(format!(
                "    cp {} {}",
                source.display(),
                destination.display()
            ));
        } else {
            report.info(format!(
                "    mkdir -p {} && curl -fL -o {} {INSTALLER_URL}",
                directory.display(),
                destination.display()
            ));
        }
        return Ok(destination);
    }

    fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;

    if let Some(source) = &opts.installer {
        if !source.exists() {
            bail!("installer not found: {}", source.display());
        }
        if source != &destination {
            fs::copy(source, &destination).with_context(|| {
                format!("copying {} to {}", source.display(), destination.display())
            })?;
        }
        return Ok(destination);
    }

    if destination
        .metadata()
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
    {
        report.info(format!("using cached installer {}", destination.display()));
        return Ok(destination);
    }

    http::download(
        report,
        INSTALLER_URL,
        &destination,
        Duration::from_secs(300),
    )?;
    Ok(destination)
}

fn dry_run(
    opts: &InstallerOptions,
    installer: &Path,
    wine: &Path,
    wine_dir: &Path,
    report: &mut Report<'_>,
) {
    let envs = wine_env(&opts.prefix, wine_dir);
    for (program, args) in [
        (wine_dir.join("bin/winecfg"), vec!["-v", "win11"]),
        (wine_dir.join("bin/wineserver"), vec!["-k"]),
    ] {
        let mut cmd = Command::new(program);
        cmd.args(args);
        report.info(format!("    {}", describe(&cmd, &envs)));
    }

    let mut cmd = Command::new(wine);
    cmd.arg(installer);
    report.info(format!("    {}", describe(&cmd, &envs)));
    report.info(format!(
        "    if {} is missing: 7z x -t# -y -o{}/.affinity-msi-<timestamp> {}",
        app_exe(&opts.prefix).display(),
        opts.prefix.display(),
        installer.display()
    ));
    let msi_log = opts.prefix.join("affinity-msi.log");
    report.info(format!(
        "    {} msiexec /i Z:\\<temp>\\<largest>.msi REBOOT=ReallySuppress INSTALL_DESKTOP_SHORTCUT_PROPERTY=#0 /l*v {}",
        wine.display(),
        to_windows_path(&msi_log)
    ));
}

fn msi_fallback(
    prefix: &Path,
    wine_dir: &Path,
    wine: &Path,
    installer: &Path,
    report: &mut Report<'_>,
) -> Result<()> {
    if which("7z").is_none() {
        bail!("7z is required to extract the Affinity MSI from the setup executable");
    }

    let temp = prefix.join(format!(".affinity-msi-{}", now_epoch()));
    if temp.exists() {
        fs::remove_dir_all(&temp).with_context(|| format!("removing stale {}", temp.display()))?;
    }
    fs::create_dir_all(&temp).with_context(|| format!("creating {}", temp.display()))?;

    let mut extract = Command::new("7z");
    extract
        .args(["x", "-t#", "-y", "-o"])
        .arg(&temp)
        .arg(installer);
    let result = run(report, extract, &[], Duration::from_secs(1800))?;
    if !result.success() {
        report.warn("7z reported warnings while extracting the MSI");
    }

    let msi = largest_msi(&temp)?;
    report.info(format!("installing embedded package {}", msi.display()));

    let envs = wine_env(prefix, wine_dir);
    let mut wait = Command::new(wine_dir.join("bin/wineserver"));
    wait.arg("-w");
    let _ = run(report, wait, &envs, Duration::from_secs(120));

    let msi_log = prefix.join("affinity-msi.log");
    let mut cmd = Command::new(wine);
    cmd.arg("msiexec")
        .arg("/i")
        .arg(to_windows_path(&msi))
        .args([
            "REBOOT=ReallySuppress",
            "INSTALL_DESKTOP_SHORTCUT_PROPERTY=#0",
            "/l*v",
        ])
        .arg(to_windows_path(&msi_log));
    let result = run(report, cmd, &envs, Duration::from_secs(3600))?;
    if !result.success() {
        report.warn(format!(
            "msiexec exited with {}; see {}",
            result.status,
            msi_log.display()
        ));
    }

    let mut wait = Command::new(wine_dir.join("bin/wineserver"));
    wait.arg("-w");
    let _ = run(report, wait, &envs, Duration::from_secs(600));

    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

fn largest_msi(directory: &Path) -> Result<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in
        fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))?
    {
        let path = entry?.path();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("msi"))
            .unwrap_or(false)
        {
            let size = path.metadata()?.len();
            if best.as_ref().map(|(best, _)| size > *best).unwrap_or(true) {
                best = Some((size, path));
            }
        }
    }
    best.map(|(_, path)| path)
        .context("no MSI package was found in the extracted setup")
}

fn kill_prefix(wine_dir: &Path, envs: &[(String, String)], report: &mut Report<'_>) -> Result<()> {
    let mut cmd = Command::new(wine_dir.join("bin/wineserver"));
    cmd.arg("-k");
    let _ = run(report, cmd, envs, Duration::from_secs(30));
    Ok(())
}

fn run(
    report: &mut Report<'_>,
    cmd: Command,
    envs: &[(String, String)],
    stall: Duration,
) -> Result<RunResult> {
    let mut options = RunOptions::default().stall(stall);
    for (key, value) in envs {
        options = options.env(key.clone(), value.clone());
    }
    runner::run(cmd, options, |stream, line| report.line(stream, line))
}

fn wine_env(prefix: &Path, wine_dir: &Path) -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_default();
    vec![
        ("WINEPREFIX".into(), prefix.display().to_string()),
        ("WINEDEBUG".into(), "-all".into()),
        ("WINESERVER_TIMEOUT".into(), "60".into()),
        (
            "PATH".into(),
            format!("{}:{path}", wine_dir.join("bin").display()),
        ),
    ]
}

fn to_windows_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

fn describe(cmd: &Command, envs: &[(String, String)]) -> String {
    let mut parts: Vec<String> = envs
        .iter()
        .filter(|(key, _)| key == "WINEPREFIX")
        .map(|(key, value)| format!("{key}={}", shell_quote(value)))
        .collect();
    parts.push(cmd.get_program().to_string_lossy().to_string());
    for arg in cmd.get_args() {
        parts.push(shell_quote(&arg.to_string_lossy()));
    }
    parts.join(" ")
}

fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+".contains(c))
    {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(prefix: &Path) -> InstallerOptions {
        InstallerOptions {
            prefix: prefix.to_path_buf(),
            installer: None,
            dry_run: true,
            log_path: None,
        }
    }

    #[test]
    fn windows_paths_use_z_drive() {
        assert_eq!(
            to_windows_path(Path::new("/tmp/a b/c.msi")),
            "Z:\\tmp\\a b\\c.msi"
        );
    }

    #[test]
    fn dry_run_prints_setup_and_msi_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = Vec::new();
        let outcome = install(&options(dir.path()), |_, line| {
            lines.push(line.to_string());
        })
        .unwrap();
        let text = lines.join("\n");
        assert!(text.contains("winecfg -v win11"));
        assert!(text.contains("Affinity-x64.exe"));
        assert!(text.contains("7z x -t#"));
        assert!(text.contains("msiexec"));
        assert!(!dir.path().join("Installer").exists());
        assert_eq!(
            outcome.installer,
            dir.path().join("Installer").join(INSTALLER_NAME)
        );
    }

    #[test]
    fn largest_msi_wins() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("small.msi"), b"tiny").unwrap();
        fs::write(dir.path().join("big.msi"), vec![0u8; 64]).unwrap();
        fs::write(dir.path().join("setup.exe"), b"ignored").unwrap();
        assert_eq!(largest_msi(dir.path()).unwrap(), dir.path().join("big.msi"));
    }

    #[test]
    fn missing_msi_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(largest_msi(dir.path()).is_err());
    }
}
