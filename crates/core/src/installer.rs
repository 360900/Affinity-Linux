//! Install Affinity v3 silently from the MSI embedded in the setup executable,
//! falling back to the interactive setup window.
//!
//! The setup (SetupUI.exe) is a wrapper that runs that same MSI, so the silent
//! path installs the same files without asking the user to click through it.

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
use crate::{WINE_DIR_NAME, app_exe, seven_zip_out};

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
    /// The silent install failed and the user went through the setup window.
    pub used_setup_ui: bool,
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
            used_setup_ui: false,
        });
    }

    let envs = wine_env(&opts.prefix, &wine_dir);
    let _ = kill_prefix(&wine_dir, &envs, &mut report);

    let mut winecfg = Command::new(wine_dir.join("bin/winecfg"));
    winecfg.args(["-v", "win11"]);
    if let Err(error) = run(&mut report, winecfg, &envs, Duration::from_secs(120)) {
        report.warn(format!("could not set Windows 11: {error}"));
    }

    let mut used_setup_ui = false;
    match msi_install(&opts.prefix, &wine_dir, &wine, &installer, &mut report) {
        Ok(()) if app_exe(&opts.prefix).exists() => {}
        Ok(()) => {
            report
                .warn("the silent install did not create Affinity.exe; opening the Affinity setup");
            used_setup_ui = true;
        }
        Err(error) => {
            report.warn(format!(
                "silent install failed: {error:#}; opening the Affinity setup"
            ));
            used_setup_ui = true;
        }
    }

    if used_setup_ui {
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
        used_setup_ui,
    })
}

/// The latest Affinity setup, from the shared download cache.
pub(crate) fn fetch_installer(report: &mut Report<'_>) -> Result<PathBuf> {
    http::download_cached(
        report,
        INSTALLER_URL,
        INSTALLER_NAME,
        Duration::from_secs(300),
    )
}

fn resolve_installer(opts: &InstallerOptions, report: &mut Report<'_>) -> Result<PathBuf> {
    let directory = opts.prefix.join("Installer");
    let Some(source) = &opts.installer else {
        if opts.dry_run {
            let cached = http::cache_dir()?.join(INSTALLER_NAME);
            report.info(format!(
                "    curl -fLR -z {0} -o {0} {INSTALLER_URL}",
                cached.display()
            ));
            return Ok(cached);
        }
        // Older versions kept a 650 MB copy inside the prefix.
        let _ = fs::remove_file(directory.join(INSTALLER_NAME));
        let _ = fs::remove_dir(&directory);
        return fetch_installer(report);
    };

    let destination = directory.join(
        source
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new(INSTALLER_NAME)),
    );
    if opts.dry_run {
        report.info(format!(
            "    cp {} {}",
            source.display(),
            destination.display()
        ));
        return Ok(destination);
    }

    fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;

    if !source.exists() {
        bail!("installer not found: {}", source.display());
    }
    if source != &destination {
        fs::copy(source, &destination).with_context(|| {
            format!("copying {} to {}", source.display(), destination.display())
        })?;
    }
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

    report.info(format!(
        "    7z x -t# -y -o{}/.affinity-msi-<timestamp> {}",
        opts.prefix.display(),
        installer.display()
    ));
    let msi_log = opts.prefix.join("affinity-msi.log");
    report.info(format!(
        "    {} msiexec /i Z:\\<temp>\\<largest>.msi /qn {} /l*v {}",
        wine.display(),
        MSI_PROPERTIES.join(" "),
        to_windows_path(&msi_log)
    ));
    let mut cmd = Command::new(wine);
    cmd.arg(installer);
    report.info(format!(
        "    if {} is missing: {}",
        app_exe(&opts.prefix).display(),
        describe(&cmd, &envs)
    ));
}

/// What SetupUI passes to msiexec, minus the install dir (the MSI default is
/// the same). No Windows desktop shortcut: we create our own launcher entry.
const MSI_PROPERTIES: &[&str] = &[
    "REBOOT=ReallySuppress",
    "INSTALL_DESKTOP_SHORTCUT_PROPERTY=#0",
];

fn msi_install(
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
        .args(["x", "-t#", "-y"])
        .arg(seven_zip_out(&temp))
        .arg(installer);
    let result = run(report, extract, &[], Duration::from_secs(1800))?;
    if !result.success() {
        report.warn("7z reported warnings while extracting the MSI");
    }

    let msi = match largest_msi(&temp) {
        Ok(msi) => msi,
        Err(error) => {
            let _ = fs::remove_dir_all(&temp);
            return Err(error);
        }
    };
    report.info(format!("installing embedded package {}", msi.display()));

    let envs = wine_env(prefix, wine_dir);
    let mut wait = Command::new(wine_dir.join("bin/wineserver"));
    wait.arg("-w");
    let _ = run(report, wait, &envs, Duration::from_secs(120));

    let msi_log = prefix.join("affinity-msi.log");
    report.info("installing silently; this can take a few minutes");
    let mut cmd = Command::new(wine);
    cmd.arg("msiexec")
        .arg("/i")
        .arg(to_windows_path(&msi))
        .arg("/qn")
        .args(MSI_PROPERTIES)
        .arg("/l*v")
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
    fn dry_run_prints_silent_msi_then_setup() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = Vec::new();
        let outcome = install(&options(dir.path()), |_, line| {
            lines.push(line.to_string());
        })
        .unwrap();
        let text = lines.join("\n");
        assert!(text.contains("winecfg -v win11"));
        assert!(text.contains("Affinity-x64.exe"));
        let msi = text.find("msiexec /i").unwrap();
        assert!(text.find("7z x -t#").unwrap() < msi);
        assert!(text[msi..].contains("/qn"));
        assert!(text.find("if ").unwrap() > msi);
        assert!(!dir.path().join("Installer").exists());
        assert_eq!(
            outcome.installer,
            http::cache_dir().unwrap().join(INSTALLER_NAME)
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
