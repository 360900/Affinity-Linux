//! Patched Wine bootstrap for the Affinity prefix.
//!
//! The prefix must always use the ElementalWarrior build that created it.
//! This module downloads the build, links it into the prefix, installs the
//! winetricks runtimes, sets Windows 11 and verifies .NET Framework 4.8.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::distro::Family;
use crate::journal::Report;
use crate::manifest::now_epoch;
use crate::runner::{self, RunOptions, RunResult, Stream};
use crate::{WINE_DIR_NAME, app_exe, deps};

pub const WINE_VERSION: &str = "11.19";
pub const WINE_FALLBACK_VERSION: &str = "11.18";
const RELEASE_TAG: &str = "11.18";
const THEME_URL: &str = "https://raw.githubusercontent.com/seapear/AffinityOnLinux/refs/heads/main/Auxiliary/Other/wine-dark-theme.reg";

/// Order matters: .NET first, then fonts and runtimes.
pub const WINETRICKS_COMPONENTS: &[&str] = &[
    "dotnet48",
    "corefonts",
    "vcrun2022",
    "msxml3",
    "msxml6",
    "tahoma",
    "renderer=vulkan",
];

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub prefix: PathBuf,
    pub family: Family,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
    /// Force a specific build. Switching inside an initialized prefix is refused.
    pub wine_version: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SetupReport {
    pub wine_version: Option<String>,
    pub wine_dir: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    pub failed_components: Vec<String>,
}

/// Download, link and configure the patched Wine build for `opts.prefix`.
pub fn setup<F>(opts: &SetupOptions, on_line: F) -> Result<SetupReport>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let prefix = &opts.prefix;

    report.info(format!("Wine setup for {}", prefix.display()));

    let mut backup = None;
    if needs_backup(prefix, opts.family) {
        let destination = backup_path(prefix);
        report.warn(format!(
            "incomplete Ubuntu-family prefix without Affinity; backing it up to {}",
            destination.display()
        ));
        if opts.dry_run {
            report.info(format!(
                "    mv {} {}",
                prefix.display(),
                destination.display()
            ));
        } else {
            kill_prefix(prefix, None, &mut report)?;
            if let Some(parent) = prefix.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            fs::rename(prefix, &destination).with_context(|| {
                format!(
                    "moving incomplete prefix {} to {}",
                    prefix.display(),
                    destination.display()
                )
            })?;
            fs::create_dir_all(prefix).with_context(|| format!("creating {}", prefix.display()))?;
            backup = Some(destination);
        }
    }

    if !opts.dry_run {
        fs::create_dir_all(prefix)
            .with_context(|| format!("creating prefix {}", prefix.display()))?;
    }

    let wine_version = ensure_wine(
        prefix,
        opts.wine_version.as_deref(),
        opts.dry_run,
        &mut report,
    )?;
    let wine_dir = prefix.join(WINE_DIR_NAME);

    if prefix.join("system.reg").exists() {
        report.info("prefix is already initialized; keeping its Wine environment");
    } else if opts.dry_run {
        report.info(format!(
            "    WINEPREFIX={} {} -u",
            prefix.display(),
            wine_dir.join("bin/wineboot").display()
        ));
    } else {
        report.info("initializing the Wine prefix");
        kill_prefix(prefix, Some(&wine_dir), &mut report)?;
        let mut cmd = Command::new(wine_dir.join("bin/wineboot"));
        cmd.arg("-u");
        let mut run_opts = stream_opts(&report.log, Some(Duration::from_secs(900)));
        run_opts.envs = wine_env(prefix, &wine_dir);
        let result = run_logged(&mut report, cmd, run_opts)?;
        if !result.success() {
            bail!("wineboot failed\n{}", result.tail_text(20));
        }
        if !prefix.join("drive_c").is_dir() {
            bail!("prefix initialization did not create drive_c");
        }
    }

    let failed_components = install_components(prefix, opts.dry_run, &mut report)?;
    configure_prefix(prefix, opts.dry_run, &mut report)?;

    if !opts.dry_run {
        let registry_path = prefix.join("system.reg");
        let registry = fs::read_to_string(&registry_path).unwrap_or_default();
        if !has_dotnet48(&registry) {
            bail!(
                ".NET Framework 4.8 is not registered in {}; Affinity SetupUI.exe crashes without it. Remove the prefix and retry, optionally with Wine {WINE_FALLBACK_VERSION}.",
                registry_path.display()
            );
        }
        report.info(".NET Framework 4.8 is registered");
    }

    Ok(SetupReport {
        wine_version: Some(wine_version),
        wine_dir: Some(wine_dir),
        backup,
        failed_components,
    })
}

/// True when a prefix must be moved aside before reinstalling Wine.
pub fn needs_backup(prefix: &Path, family: Family) -> bool {
    family == Family::Ubuntu && prefix.join("system.reg").exists() && !app_exe(prefix).exists()
}

/// Sibling backup path that does not collide with an earlier backup.
pub fn backup_path(prefix: &Path) -> PathBuf {
    let parent = prefix.parent().unwrap_or_else(|| Path::new("."));
    let name = prefix
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "prefix".to_string());
    let base = format!("{name}.backup-{}", now_epoch());
    let mut candidate = parent.join(&base);
    let mut suffix = 1;
    while candidate.exists() {
        candidate = parent.join(format!("{base}-{suffix}"));
        suffix += 1;
    }
    candidate
}

/// .NET Framework 4.8 detection against the prefix's `system.reg` text.
pub fn has_dotnet48(system_reg: &str) -> bool {
    let mut in_block = false;
    let mut install = false;
    let mut release = false;
    let mut version = false;

    for line in system_reg.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_block && install && (release || version) {
                return true;
            }
            in_block = is_dotnet48_header(trimmed);
            install = false;
            release = false;
            version = false;
            continue;
        }
        if !in_block {
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("\"install\"=dword:") {
            install = lower.ends_with("00000001");
        } else if let Some(value) = lower.strip_prefix("\"release\"=dword:") {
            if let Ok(value) = u32::from_str_radix(value.trim(), 16) {
                release = value >= 0x80e18;
            }
        } else if let Some(value) = lower.strip_prefix("\"version\"=\"") {
            version = value.starts_with("4.8");
        }
    }

    in_block && install && (release || version)
}

fn is_dotnet48_header(line: &str) -> bool {
    line.to_ascii_lowercase()
        .contains("net framework setup\\\\ndp\\\\v4\\\\full]")
}

fn install_components(prefix: &Path, dry_run: bool, report: &mut Report) -> Result<Vec<String>> {
    if !dry_run && deps::which("winetricks").is_none() {
        bail!("winetricks was not found on PATH; run `affinity-linux deps` first");
    }

    let wine_dir = prefix.join(WINE_DIR_NAME);
    let mut failed = Vec::new();
    let total = WINETRICKS_COMPONENTS.len();

    report.info("installing Winetricks runtimes (this can take 10-25 minutes)");
    for (index, component) in WINETRICKS_COMPONENTS.iter().enumerate() {
        report.info(format!("[{}/{}] winetricks {component}", index + 1, total));

        let cmd = winetricks_command(component);
        if dry_run {
            report.info(format!("    {}", describe(&cmd)));
            continue;
        }

        kill_prefix(prefix, Some(&wine_dir), report)?;
        let mut run_opts = stream_opts(&report.log, Some(Duration::from_secs(1200)));
        run_opts.envs = wine_env(prefix, &wine_dir);
        let result = run_logged(report, cmd, run_opts)?;

        if result.success() {
            report.info(format!("{component} installed"));
        } else {
            let reason = if result.stalled {
                "stalled and was stopped"
            } else {
                "failed"
            };
            report.warn(format!("{component} {reason}; continuing"));
            failed.push(component.to_string());
            kill_prefix(prefix, Some(&wine_dir), report)?;
        }
    }

    Ok(failed)
}

fn configure_prefix(prefix: &Path, dry_run: bool, report: &mut Report) -> Result<()> {
    let wine_dir = prefix.join(WINE_DIR_NAME);

    report.info("setting Windows version to 11");
    let winecfg = wine_dir.join("bin/winecfg");
    if dry_run {
        report.info(format!("    {} -v win11", winecfg.display()));
    } else if !winecfg.is_file() {
        report.warn(format!("{} is missing", winecfg.display()));
    } else {
        kill_prefix(prefix, Some(&wine_dir), report)?;
        let mut cmd = Command::new(&winecfg);
        cmd.args(["-v", "win11"]);
        let mut run_opts = stream_opts(&report.log, Some(Duration::from_secs(300)));
        run_opts.envs = wine_env(prefix, &wine_dir);
        let result = run_logged(report, cmd, run_opts)?;
        if !result.success() {
            report.warn("winecfg failed to set Windows 11; continuing");
        }
    }

    report.info("applying the Wine dark theme");
    let theme_path = prefix.join("wine-dark-theme.reg");
    if dry_run {
        report.info(format!(
            "    curl -fL {THEME_URL} -o {}",
            theme_path.display()
        ));
        report.info(format!(
            "    {} {}",
            wine_dir.join("bin/regedit").display(),
            theme_path.display()
        ));
        return Ok(());
    }

    if let Err(error) = download(report, THEME_URL, &theme_path) {
        report.warn(format!("dark theme download failed: {error:#}"));
        return Ok(());
    }

    let regedit = wine_dir.join("bin/regedit");
    if regedit.is_file() {
        let mut cmd = Command::new(&regedit);
        cmd.arg(&theme_path);
        let mut run_opts = stream_opts(&report.log, Some(Duration::from_secs(120)));
        run_opts.envs = wine_env(prefix, &wine_dir);
        let result = run_logged(report, cmd, run_opts)?;
        if !result.success() {
            report.warn("dark theme import failed; continuing");
        }
    }
    let _ = fs::remove_file(&theme_path);
    Ok(())
}

fn winetricks_command(component: &str) -> Command {
    let mut cmd = Command::new("winetricks");
    cmd.args(["--unattended", "--no-isolate", "--optout"]);
    let verb = component.split('=').next().unwrap_or(component);
    if !verb.starts_with("dotnet") {
        cmd.arg("--force");
    }
    cmd.arg(component);
    cmd
}

fn ensure_wine(
    prefix: &Path,
    requested: Option<&str>,
    dry_run: bool,
    report: &mut Report,
) -> Result<String> {
    if let Some(existing) = detect_installed_build(prefix) {
        let compatible = requested
            .map(|requested| existing.starts_with(requested))
            .unwrap_or(true);
        if compatible {
            report.info(format!("using existing Wine {existing}"));
            return Ok(existing);
        }

        if prefix.join("system.reg").exists() {
            bail!(
                "prefix already uses Wine {existing}; refusing to switch builds inside an initialized prefix. Remove {} and rerun with the requested version.",
                prefix.display()
            );
        }
        report.warn(format!(
            "replacing Wine {existing} with {} in an uninitialized prefix",
            requested.unwrap_or(WINE_VERSION)
        ));
    }

    install_wine(prefix, requested, dry_run, report)
}

fn detect_installed_build(prefix: &Path) -> Option<String> {
    let link = prefix.join(WINE_DIR_NAME);
    if !link.join("bin/wine").is_file() {
        return None;
    }
    let target = fs::read_link(&link).ok()?;
    let name = target.file_name()?.to_string_lossy();
    let version = name.strip_prefix("ElementalWarrior-wine-")?;
    Some(version.to_string())
}

fn install_wine(
    prefix: &Path,
    requested: Option<&str>,
    dry_run: bool,
    report: &mut Report,
) -> Result<String> {
    let candidates = match requested {
        Some(version) => {
            release_for(version)?;
            vec![version.to_string()]
        }
        None => vec![WINE_VERSION.to_string(), WINE_FALLBACK_VERSION.to_string()],
    };

    if let Some(cached) = candidates
        .iter()
        .find(|version| find_wine_dir(prefix, version).is_some())
    {
        let dir = find_wine_dir(prefix, cached).expect("checked above");
        report.info(format!("found extracted Wine {cached}"));
        if dry_run {
            report.info(format!(
                "    ln -s {} {}",
                dir.display(),
                prefix.join(WINE_DIR_NAME).display()
            ));
        } else {
            link_wine(prefix, &dir)?;
            verify_wine(prefix, dry_run, report)?;
        }
        return Ok(cached.clone());
    }

    if dry_run {
        let release = release_for(&candidates[0])?;
        report.info(format!(
            "    curl -fL {} -o {}",
            release.url,
            prefix.join(&release.file_name).display()
        ));
        report.info(format!(
            "    tar -xJf {} -C {} --no-same-owner",
            prefix.join(&release.file_name).display(),
            prefix.display()
        ));
        report.info(format!(
            "    ln -s {} {}",
            release.dir_pattern,
            prefix.join(WINE_DIR_NAME).display()
        ));
        report.info(format!(
            "    {} --version",
            prefix.join(WINE_DIR_NAME).join("bin/wine").display()
        ));
        if candidates.len() > 1 {
            report.info(format!(
                "    fallback: Wine {} if {} fails",
                candidates[1], candidates[0]
            ));
        }
        return Ok(candidates[0].clone());
    }

    let mut errors = Vec::new();
    for version in &candidates {
        let release = release_for(version)?;
        let archive = prefix.join(&release.file_name);

        if let Some(dir) = find_wine_dir(prefix, version) {
            report.info(format!("found extracted Wine {version}"));
            link_wine(prefix, &dir)?;
            verify_wine(prefix, dry_run, report)?;
            return Ok(version.clone());
        }

        report.info(format!("downloading Wine {version}"));
        if let Err(error) = download(report, &release.url, &archive) {
            report.warn(format!("Wine {version} download failed: {error:#}"));
            let _ = fs::remove_file(&archive);
            errors.push(format!("{version}: download failed"));
            continue;
        }

        report.info(format!("extracting Wine {version}"));
        let mut cmd = Command::new("tar");
        cmd.args(["-xJf"])
            .arg(&archive)
            .arg("-C")
            .arg(prefix)
            .args(["--no-same-owner", "--overwrite"]);
        let run_opts = stream_opts(&report.log, Some(Duration::from_secs(1800)));
        let result = run_logged(report, cmd, run_opts)?;
        let _ = fs::remove_file(&archive);
        if !result.success() {
            report.warn(format!(
                "Wine {version} extraction failed\n{}",
                result.tail_text(10)
            ));
            errors.push(format!("{version}: extraction failed"));
            continue;
        }

        let Some(dir) = find_wine_dir(prefix, version) else {
            report.warn(format!(
                "Wine {version} archive did not contain {}",
                release.dir_pattern
            ));
            errors.push(format!("{version}: archive layout not recognized"));
            continue;
        };

        link_wine(prefix, &dir)?;
        verify_wine(prefix, dry_run, report)?;
        return Ok(version.clone());
    }

    bail!(
        "could not install a working Wine build ({})",
        errors.join("; ")
    )
}

fn release_for(version: &str) -> Result<Release> {
    if version != WINE_VERSION && version != WINE_FALLBACK_VERSION {
        bail!(
            "unsupported Wine version '{version}' (expected {WINE_VERSION} or {WINE_FALLBACK_VERSION})"
        );
    }
    Ok(Release {
        dir_pattern: format!("ElementalWarrior-wine-{version}"),
        file_name: format!("ElementalWarrior-wine-{version}.tar.xz"),
        url: format!(
            "https://github.com/ryzendew/Affinity-Wine-Builder/releases/download/{RELEASE_TAG}/ElementalWarrior-wine-{version}.tar.xz"
        ),
    })
}

struct Release {
    dir_pattern: String,
    file_name: String,
    url: String,
}

fn find_wine_dir(prefix: &Path, version: &str) -> Option<PathBuf> {
    let pattern = format!("ElementalWarrior-wine-{version}");
    let entries = fs::read_dir(prefix).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && entry.file_name().to_string_lossy().starts_with(&pattern)
            && path.join("bin/wine").is_file()
        {
            return Some(path);
        }
    }
    None
}

fn link_wine(prefix: &Path, extracted: &Path) -> Result<()> {
    let link = prefix.join(WINE_DIR_NAME);
    if let Ok(metadata) = link.symlink_metadata() {
        if metadata.file_type().is_symlink() || metadata.is_file() {
            fs::remove_file(&link)
                .with_context(|| format!("removing old Wine link {}", link.display()))?;
        } else if metadata.is_dir() {
            if link.join("bin/wine").is_file() {
                return Ok(());
            }
            bail!(
                "{} exists and is not a usable Wine build; remove it manually",
                link.display()
            );
        }
    }

    let target = extracted.strip_prefix(prefix).unwrap_or(extracted);
    std::os::unix::fs::symlink(target, &link)
        .with_context(|| format!("linking {} to {}", link.display(), target.display()))
}

fn verify_wine(prefix: &Path, dry_run: bool, report: &mut Report) -> Result<()> {
    let wine = prefix.join(WINE_DIR_NAME).join("bin/wine");
    if !wine.is_file() {
        bail!("Wine binary is missing at {}", wine.display());
    }
    if dry_run {
        report.info(format!("    {} --version", wine.display()));
        return Ok(());
    }

    let mut cmd = Command::new(&wine);
    cmd.arg("--version");
    let mut run_opts = stream_opts(&report.log, Some(Duration::from_secs(120)));
    run_opts.envs = wine_env(prefix, &prefix.join(WINE_DIR_NAME));
    let result = run_logged(report, cmd, run_opts)?;
    if !result.success() {
        bail!(
            "Wine binary at {} is not functional\n{}",
            wine.display(),
            result.tail_text(10)
        );
    }
    Ok(())
}

fn download(report: &mut Report, url: &str, destination: &Path) -> Result<()> {
    if destination.exists() {
        let _ = fs::remove_file(destination);
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let cmd = if deps::which("curl").is_some() {
        let mut cmd = Command::new("curl");
        cmd.args([
            "-fL",
            "--retry",
            "3",
            "--retry-delay",
            "2",
            "--connect-timeout",
            "30",
            "--progress-bar",
            "-o",
        ]);
        cmd.arg(destination).arg(url);
        cmd
    } else if deps::which("wget").is_some() {
        let mut cmd = Command::new("wget");
        cmd.args(["--tries=3", "-O"]).arg(destination).arg(url);
        cmd
    } else {
        bail!("neither curl nor wget was found on PATH");
    };

    let run_opts = stream_opts(&report.log, None);
    let result = run_logged(report, cmd, run_opts)?;
    if !result.success() {
        bail!("download failed\n{}", result.tail_text(10));
    }
    if !destination.is_file() {
        bail!("download did not create {}", destination.display());
    }
    Ok(())
}

fn kill_prefix(prefix: &Path, wine_dir: Option<&Path>, report: &mut Report) -> Result<()> {
    let wineserver = wine_dir
        .map(|dir| dir.join("bin/wineserver"))
        .filter(|path| path.is_file())
        .or_else(|| deps::which("wineserver"));
    let Some(wineserver) = wineserver else {
        return Ok(());
    };

    let mut cmd = Command::new(wineserver);
    cmd.arg("-k");
    let mut run_opts = stream_opts(&report.log, Some(Duration::from_secs(30)));
    run_opts.echo = false;
    run_opts
        .envs
        .push(("WINEPREFIX".into(), prefix.display().to_string()));
    let _ = run_logged(report, cmd, run_opts)?;
    Ok(())
}

fn wine_env(prefix: &Path, wine_dir: &Path) -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_default();
    vec![
        ("WINEPREFIX".into(), prefix.display().to_string()),
        ("WINEDEBUG".into(), "-all,fixme-all".into()),
        ("WINEDLLOVERRIDES".into(), "mscoree=;mshtml=".into()),
        ("WINESERVER_TIMEOUT".into(), "60".into()),
        ("WINETRICKS_GUI".into(), "none".into()),
        (
            "PATH".into(),
            format!("{}:{path}", wine_dir.join("bin").display()),
        ),
    ]
}

fn stream_opts(log: &Option<PathBuf>, stall: Option<Duration>) -> RunOptions {
    let mut options = RunOptions::default();
    if let Some(stall) = stall {
        options = options.stall(stall);
    }
    if let Some(path) = log {
        options = options.log(path).log_append();
    }
    options
}

fn run_logged(report: &mut Report, cmd: Command, opts: RunOptions) -> Result<RunResult> {
    runner::run(cmd, opts, |stream, line| report.line(stream, line))
}

fn describe(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().to_string()];
    for arg in cmd.get_args() {
        parts.push(shell_quote(&arg.to_string_lossy()));
    }
    parts.join(" ")
}

fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }
    if text
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

    fn options(prefix: &Path) -> SetupOptions {
        SetupOptions {
            prefix: prefix.to_path_buf(),
            family: Family::Arch,
            dry_run: true,
            log_path: None,
            wine_version: None,
        }
    }

    #[test]
    fn dotnet48_registry_detection() {
        let good = r#"
[Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full] 1700000000
#time=1d9c0c0c0c0c0c0
"Install"=dword:00000001
"Release"=dword:00080e18
"Version"="4.8.03761"

[Software\\Microsoft\\Windows\\CurrentVersion]
"Version"="10.0"
"#;
        assert!(has_dotnet48(good));

        let wow = r#"
[Software\\Wow6432Node\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full] 1700000000
"Install"=dword:00000001
"Version"="4.8.03761"
"#;
        assert!(has_dotnet48(wow));

        let not_installed = r#"
[Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full] 1700000000
"Install"=dword:00000000
"Version"="4.8.03761"
"#;
        assert!(!has_dotnet48(not_installed));

        let too_old = r#"
[Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full] 1700000000
"Install"=dword:00000001
"Release"=dword:00000001
"Version"="4.7.03190"
"#;
        assert!(!has_dotnet48(too_old));

        let subkey_only = r#"
[Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full\\Something] 1700000000
"Install"=dword:00000001
"Version"="4.8.03761"
"#;
        assert!(!has_dotnet48(subkey_only));
        assert!(!has_dotnet48(""));
    }

    #[test]
    fn winetricks_force_is_skipped_for_dotnet() {
        let dotnet: Vec<String> = winetricks_command("dotnet48")
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert!(!dotnet.iter().any(|arg| arg == "--force"));
        assert!(dotnet.contains(&"dotnet48".to_string()));

        let fonts: Vec<String> = winetricks_command("corefonts")
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert!(fonts.iter().any(|arg| arg == "--force"));

        let renderer: Vec<String> = winetricks_command("renderer=vulkan")
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert!(renderer.iter().any(|arg| arg == "--force"));
    }

    #[test]
    fn release_assets_match_the_upstream_tag() {
        let latest = release_for("11.19").unwrap();
        assert_eq!(latest.file_name, "ElementalWarrior-wine-11.19.tar.xz");
        assert_eq!(latest.dir_pattern, "ElementalWarrior-wine-11.19");
        assert!(latest.url.contains("/releases/download/11.18/"));
        assert!(release_for("11.20").is_err());
    }

    #[test]
    fn dry_run_does_not_touch_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        let mut lines = Vec::new();

        let report = setup(&options(&prefix), |_, line| lines.push(line.to_string())).unwrap();

        assert!(!prefix.exists());
        assert_eq!(report.wine_version.as_deref(), Some("11.19"));
        let text = lines.join("\n");
        assert!(text.contains("ElementalWarrior-wine-11.19.tar.xz"));
        assert!(text.contains("wineboot"));
        assert!(text.contains("dotnet48"));
        assert!(text.contains("corefonts"));
        assert!(text.contains("renderer=vulkan"));
        assert!(text.contains("winecfg"));
        assert!(text.contains("regedit"));
    }

    #[test]
    fn detects_build_from_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let build = prefix.join("ElementalWarrior-wine-11.19/bin");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("wine"), "fake").unwrap();
        std::os::unix::fs::symlink("ElementalWarrior-wine-11.19", prefix.join(WINE_DIR_NAME))
            .unwrap();

        assert_eq!(detect_installed_build(prefix).as_deref(), Some("11.19"));
    }

    #[test]
    fn detects_extracted_build() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let build = prefix.join("ElementalWarrior-wine-11.18/bin");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("wine"), "fake").unwrap();

        assert!(find_wine_dir(prefix, "11.18").is_some());
        assert!(find_wine_dir(prefix, "11.19").is_none());
    }

    #[test]
    fn backup_only_applies_to_incomplete_ubuntu_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        fs::write(prefix.join("system.reg"), "WINE REGISTRY").unwrap();

        assert!(needs_backup(&prefix, Family::Ubuntu));
        assert!(!needs_backup(&prefix, Family::Arch));

        let exe = app_exe(&prefix);
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "MZ").unwrap();
        assert!(!needs_backup(&prefix, Family::Ubuntu));
    }

    #[test]
    fn backup_paths_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join(".AffinityLinux");
        fs::create_dir_all(&prefix).unwrap();

        let first = backup_path(&prefix);
        fs::create_dir_all(&first).unwrap();
        let second = backup_path(&prefix);

        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".backup-")
        );
    }
}
