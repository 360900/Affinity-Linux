//! GPU backend installation: vkd3d-proton or DXVK.
//!
//! vkd3d-proton provides the d3d12 runtime used by Affinity. DXVK provides the
//! d3d8/d3d9/d3d10/d3d11 stack. Either way we make sure the DLL overrides point
//! at the native DLLs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::{self, GpuBackend};
use crate::deps::which;
use crate::http;
use crate::journal::Report;
use crate::manifest::now_epoch;
use crate::runner::{self, RunOptions, Stream};
use crate::{WINE_DIR_NAME, app_exe};

pub const VKD3D_REPO: &str = "HansKristian-Work/vkd3d-proton";
pub const VKD3D_FALLBACK_VERSION: &str = "3.0a";
pub const DXVK_REPO: &str = "doitsujin/dxvk";
pub const DXVK_FALLBACK_VERSION: &str = "2.3";
pub const DXVK_DLLS: [&str; 7] = [
    "d3d8.dll",
    "d3d9.dll",
    "d3d10.dll",
    "d3d10_1.dll",
    "d3d10core.dll",
    "d3d11.dll",
    "dxgi.dll",
];

#[derive(Debug, Clone)]
pub struct BackendOptions {
    pub prefix: PathBuf,
    pub backend: GpuBackend,
    pub opencl: bool,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct BackendOutcome {
    pub backend: GpuBackend,
    pub vkd3d_version: Option<String>,
    pub dxvk_version: Option<String>,
    pub opencl: bool,
}

pub fn install<F>(opts: &BackendOptions, on_line: F) -> Result<BackendOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let wine_dir = opts.prefix.join(WINE_DIR_NAME);
    let wine = wine_dir.join("bin/wine");
    let mut outcome = BackendOutcome {
        backend: opts.backend,
        opencl: opts.opencl,
        ..BackendOutcome::default()
    };

    report.info(format!(
        "GPU backend for {}: {}",
        opts.prefix.display(),
        backend_name(opts.backend)
    ));

    if opts.dry_run {
        dry_run(opts, &mut report);
        return Ok(outcome);
    }

    if !wine.exists() {
        bail!(
            "Wine is not installed at {}; run wine-setup first",
            wine.display()
        );
    }

    let envs = wine_env(&opts.prefix, &wine_dir);
    let _ = kill_prefix(&wine_dir, &envs, &mut report);

    match opts.backend {
        GpuBackend::Vkd3d => match install_vkd3d(&opts.prefix, &wine_dir, &mut report) {
            Ok(version) => {
                outcome.vkd3d_version = version;
                let wine_lib = wine_dir.join("lib/wine/vkd3d-proton/x86_64-windows");
                if wine_lib.join("d3d12.dll").exists() {
                    for dll in ["d3d12", "d3d12core"] {
                        set_override(&mut report, &wine, &envs, dll);
                    }
                }
            }
            Err(error) => report.warn(format!("could not install vkd3d-proton: {error}")),
        },
        GpuBackend::Dxvk => match install_dxvk(&opts.prefix, &wine_dir, &mut report) {
            Ok(version) => outcome.dxvk_version = version,
            Err(error) => report.warn(format!("could not install DXVK: {error}")),
        },
    }

    let _ = kill_prefix(&wine_dir, &envs, &mut report);

    let _ = config::write_prefix_state(
        &opts.prefix,
        ".dxvk_vkd3d_preference",
        backend_name(opts.backend),
    );
    let _ = config::write_prefix_state(
        &opts.prefix,
        ".opencl_enabled",
        if opts.opencl { "1" } else { "0" },
    );
    if config::read_prefix_state(&opts.prefix, ".gpu_config").is_none() {
        let _ = config::write_prefix_state(&opts.prefix, ".gpu_config", "auto");
    }

    report.info(format!(
        "GPU backend configured: {}{}",
        backend_name(opts.backend),
        if opts.opencl { " (OpenCL)" } else { "" }
    ));
    Ok(outcome)
}

/// Prefer the hook when the plugin loader is installed.
pub fn launch_target(prefix: &Path) -> PathBuf {
    let exe = app_exe(prefix);
    let hook = exe.with_file_name("AffinityHook.exe");
    if hook.exists() { hook } else { exe }
}

/// Copy the d3d12 runtime next to the Affinity executable when it is missing
/// or stale. Returns true when files were copied.
pub fn sync_app_dlls(prefix: &Path) -> Result<bool> {
    let app_dir = match app_exe(prefix).parent() {
        Some(dir) => dir.to_path_buf(),
        None => return Ok(false),
    };
    if !app_dir.exists() {
        return Ok(false);
    }
    let wine_lib = prefix
        .join(WINE_DIR_NAME)
        .join("lib/wine/vkd3d-proton/x86_64-windows");
    let staging = prefix.join("vkd3d_dlls");
    let mut copied = false;

    for dll in ["d3d12.dll", "d3d12core.dll"] {
        let target = app_dir.join(dll);
        let source = [staging.join(dll), wine_lib.join(dll)]
            .into_iter()
            .find(|path| path.exists());
        let Some(source) = source else {
            continue;
        };
        let stale = match (fs::metadata(&target), fs::metadata(&source)) {
            (Ok(target), Ok(source)) => target.len() != source.len(),
            _ => true,
        };
        if stale {
            fs::copy(&source, &target)
                .with_context(|| format!("copying {} to {}", source.display(), target.display()))?;
            copied = true;
        }
    }
    Ok(copied)
}

fn dry_run(opts: &BackendOptions, report: &mut Report<'_>) {
    let wine_dir = opts.prefix.join(WINE_DIR_NAME);
    let wine = wine_dir.join("bin/wine");
    match opts.backend {
        GpuBackend::Vkd3d => {
            report.info(format!(
                "would download https://github.com/{VKD3D_REPO}/releases/download/v{VKD3D_FALLBACK_VERSION}/vkd3d-proton-{VKD3D_FALLBACK_VERSION}.tar.zst (latest release at run time)"
            ));
            report.info(format!(
                "would extract the archive to {}",
                opts.prefix.join(".cache").display()
            ));
            for dll in ["d3d12.dll", "d3d12core.dll"] {
                report.info(format!(
                    "would copy {dll} to {} and {}",
                    opts.prefix.join("vkd3d_dlls").display(),
                    wine_dir
                        .join("lib/wine/vkd3d-proton/x86_64-windows")
                        .display()
                ));
            }
            for dll in ["d3d12", "d3d12core"] {
                report.info(format!(
                    "{} reg add HKCU\\Software\\Wine\\DllOverrides /v {dll} /d native,builtin /f",
                    wine.display()
                ));
            }
        }
        GpuBackend::Dxvk => {
            report.info("winetricks --unattended --no-isolate --optout --force dxvk");
            report.info(format!(
                "fallback: download https://github.com/{DXVK_REPO}/releases/download/v{DXVK_FALLBACK_VERSION}/dxvk-{DXVK_FALLBACK_VERSION}.tar.gz and copy x64 DLLs to drive_c/windows/system32"
            ));
            for dll in DXVK_DLLS {
                report.info(format!(
                    "{} reg add HKCU\\Software\\Wine\\DllOverrides /v {} /d native,builtin /f",
                    wine.display(),
                    dll.trim_end_matches(".dll")
                ));
            }
        }
    }
    report.info(format!(
        "would write {} = {}",
        opts.prefix.join(".dxvk_vkd3d_preference").display(),
        backend_name(opts.backend)
    ));
    report.info(format!(
        "would write {} = {}",
        opts.prefix.join(".opencl_enabled").display(),
        if opts.opencl { "1" } else { "0" }
    ));
}

fn install_vkd3d(
    prefix: &Path,
    _wine_dir: &Path,
    report: &mut Report<'_>,
) -> Result<Option<String>> {
    let latest = latest_version(VKD3D_REPO).unwrap_or_else(|| VKD3D_FALLBACK_VERSION.to_string());
    let mut candidates = vec![latest.clone()];
    if latest != VKD3D_FALLBACK_VERSION {
        candidates.push(VKD3D_FALLBACK_VERSION.to_string());
    }

    let mut last_error = None;
    for version in candidates {
        match ensure_vkd3d(prefix, &version, report) {
            Ok(()) => return Ok(Some(version)),
            Err(error) => {
                report.warn(format!("vkd3d-proton {version}: {error}"));
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no vkd3d-proton release available")))
}

fn ensure_vkd3d(prefix: &Path, version: &str, report: &mut Report<'_>) -> Result<()> {
    let cache = prefix.join(".cache");
    let root = cache.join(format!("vkd3d-proton-{version}"));
    let stored = config::read_prefix_state(prefix, ".vkd3d_version");
    let wine_lib = prefix
        .join(WINE_DIR_NAME)
        .join("lib/wine/vkd3d-proton/x86_64-windows");
    let already = stored.as_deref() == Some(version)
        && wine_lib.join("d3d12.dll").exists()
        && wine_lib.join("d3d12core.dll").exists();

    if already {
        report.info(format!("vkd3d-proton {version} is already installed"));
    } else {
        if !root.join("x64/d3d12.dll").exists() {
            let archive = cache.join(format!("vkd3d-proton-{version}.tar.zst"));
            if !archive.exists() {
                let url = format!(
                    "https://github.com/{VKD3D_REPO}/releases/download/v{version}/vkd3d-proton-{version}.tar.zst"
                );
                http::download(report, &url, &archive, Duration::from_secs(900))?;
            }
            extract_tar_zst(&cache, &archive, report)?;
            let _ = fs::remove_file(&archive);
        }

        let source = dll_dir(&root);
        let d3d12 = source.join("d3d12.dll");
        let core = source.join("d3d12core.dll");
        if !d3d12.exists() || !core.exists() {
            bail!("archive is missing d3d12.dll or d3d12core.dll");
        }

        fs::create_dir_all(&wine_lib)
            .with_context(|| format!("creating {}", wine_lib.display()))?;
        let staging = prefix.join("vkd3d_dlls");
        fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
        for source in [&d3d12, &core] {
            let name = source.file_name().context("DLL name")?;
            fs::copy(source, wine_lib.join(name))
                .with_context(|| format!("copying {}", source.display()))?;
            fs::copy(source, staging.join(name))
                .with_context(|| format!("copying {}", source.display()))?;
        }
        config::write_prefix_state(prefix, ".vkd3d_version", version)?;
        report.info(format!("installed vkd3d-proton {version}"));
    }

    // Always make sure the staging copies exist for the launch-time sync.
    let staging = prefix.join("vkd3d_dlls");
    fs::create_dir_all(&staging)?;
    for dll in ["d3d12.dll", "d3d12core.dll"] {
        if !staging.join(dll).exists() && wine_lib.join(dll).exists() {
            fs::copy(wine_lib.join(dll), staging.join(dll))?;
        }
    }
    Ok(())
}

fn install_dxvk(prefix: &Path, wine_dir: &Path, report: &mut Report<'_>) -> Result<Option<String>> {
    let envs = wine_env(prefix, wine_dir);
    let system32 = prefix.join("drive_c/windows/system32");
    let _ = kill_prefix(wine_dir, &envs, report);

    if which("winetricks").is_some() {
        report.info("installing DXVK via winetricks");
        let mut cmd = Command::new("winetricks");
        cmd.args([
            "--unattended",
            "--no-isolate",
            "--optout",
            "--force",
            "dxvk",
        ]);
        match run(report, cmd, &envs, Duration::from_secs(3600)) {
            Ok(result) if result.success() => {}
            Ok(result) => report.warn(format!(
                "winetricks dxvk exited with {}; verifying DLLs",
                result.status
            )),
            Err(error) => report.warn(format!("winetricks dxvk failed: {error}")),
        }
        let _ = kill_prefix(wine_dir, &envs, report);
    } else {
        report.warn("winetricks not found; installing DXVK manually");
    }

    let missing: Vec<&str> = DXVK_DLLS
        .iter()
        .copied()
        .filter(|dll| !system32.join(dll).exists())
        .collect();

    let mut version = config::read_prefix_state(prefix, ".dxvk_version");
    if !missing.is_empty() {
        let latest = latest_version(DXVK_REPO).unwrap_or_else(|| DXVK_FALLBACK_VERSION.to_string());
        let mut candidates = vec![latest.clone()];
        if latest != DXVK_FALLBACK_VERSION {
            candidates.push(DXVK_FALLBACK_VERSION.to_string());
        }
        let mut last_error = None;
        let mut installed = false;
        for candidate in candidates {
            match ensure_dxvk(prefix, &candidate, &missing, report) {
                Ok(()) => {
                    version = Some(candidate);
                    installed = true;
                    break;
                }
                Err(error) => {
                    report.warn(format!("DXVK {candidate}: {error}"));
                    last_error = Some(error);
                }
            }
        }
        if !installed {
            return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no DXVK release available")));
        }
    } else {
        report.info("DXVK DLLs verified in system32");
    }

    for dll in DXVK_DLLS {
        set_override(
            report,
            &wine_dir.join("bin/wine"),
            &envs,
            dll.trim_end_matches(".dll"),
        );
    }

    Ok(version)
}

fn ensure_dxvk(
    prefix: &Path,
    version: &str,
    missing: &[&str],
    report: &mut Report<'_>,
) -> Result<()> {
    let cache = prefix.join(".cache");
    let root = cache.join(format!("dxvk-{version}"));
    if !root.join("x64/d3d9.dll").exists() {
        let archive = cache.join(format!("dxvk-{version}.tar.gz"));
        if !archive.exists() {
            let url = format!(
                "https://github.com/{DXVK_REPO}/releases/download/v{version}/dxvk-{version}.tar.gz"
            );
            http::download(report, &url, &archive, Duration::from_secs(600))?;
        }
        fs::create_dir_all(&cache)?;
        let mut tar = Command::new("tar");
        tar.arg("-xzf").arg(&archive).arg("-C").arg(&cache);
        run_checked(
            report,
            tar,
            &[],
            Duration::from_secs(300),
            "extracting DXVK",
        )?;
        let _ = fs::remove_file(&archive);
    }

    let source = dll_dir(&root);
    let system32 = prefix.join("drive_c/windows/system32");
    fs::create_dir_all(&system32)?;
    let mut copied = 0;
    for dll in missing {
        let from = source.join(dll);
        if from.exists() {
            fs::copy(&from, system32.join(dll))
                .with_context(|| format!("copying {}", from.display()))?;
            copied += 1;
        }
    }
    if copied == 0 {
        bail!("no matching DLLs found in the DXVK archive");
    }
    config::write_prefix_state(prefix, ".dxvk_version", version)?;
    report.info(format!("installed {copied} DXVK DLL(s) into system32"));
    Ok(())
}

fn dll_dir(root: &Path) -> PathBuf {
    let x64 = root.join("x64");
    if x64.is_dir() {
        x64
    } else {
        root.to_path_buf()
    }
}

fn extract_tar_zst(destination: &Path, archive: &Path, report: &mut Report<'_>) -> Result<()> {
    fs::create_dir_all(destination)?;
    let tar_path = destination.join(format!("affinity-vkd3d-{}.tar", now_epoch()));
    let decompress = if which("unzstd").is_some() {
        let mut cmd = Command::new("unzstd");
        cmd.arg("-f").arg(archive).arg("-o").arg(&tar_path);
        cmd
    } else if which("zstd").is_some() {
        let mut cmd = Command::new("zstd");
        cmd.args(["-d", "-f"]).arg(archive).arg("-o").arg(&tar_path);
        cmd
    } else {
        bail!("zstd is required to extract vkd3d-proton");
    };
    run_checked(
        report,
        decompress,
        &[],
        Duration::from_secs(300),
        "decompressing vkd3d-proton",
    )?;

    let mut tar = Command::new("tar");
    tar.arg("-xf").arg(&tar_path).arg("-C").arg(destination);
    let result = run_checked(
        report,
        tar,
        &[],
        Duration::from_secs(300),
        "extracting vkd3d-proton",
    );
    let _ = fs::remove_file(&tar_path);
    result
}

fn set_override(report: &mut Report<'_>, wine: &Path, envs: &[(String, String)], name: &str) {
    let mut cmd = Command::new(wine);
    cmd.args([
        "reg",
        "add",
        "HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides",
        "/v",
        name,
        "/t",
        "REG_SZ",
        "/d",
        "native,builtin",
        "/f",
    ]);
    match run(report, cmd, envs, Duration::from_secs(60)) {
        Ok(result) if result.success() => {}
        Ok(result) => report.warn(format!(
            "could not set DLL override {name}: {}",
            result.status
        )),
        Err(error) => report.warn(format!("could not set DLL override {name}: {error}")),
    }
}

fn wine_env(prefix: &Path, wine_dir: &Path) -> Vec<(String, String)> {
    let path = std::env::var("PATH").unwrap_or_default();
    vec![
        ("WINEPREFIX".into(), prefix.display().to_string()),
        ("WINEDEBUG".into(), "-all,fixme-all".into()),
        ("WINESERVER_TIMEOUT".into(), "60".into()),
        ("WINETRICKS_GUI".into(), "none".into()),
        ("PATH".into(), format!("{}/bin:{path}", wine_dir.display())),
    ]
}

fn kill_prefix(wine_dir: &Path, envs: &[(String, String)], report: &mut Report<'_>) -> Result<()> {
    let wineserver = wine_dir.join("bin/wineserver");
    let mut cmd = if wineserver.exists() {
        Command::new(wineserver)
    } else {
        Command::new("wineserver")
    };
    cmd.arg("-k");
    let mut opts = RunOptions::default().stall(Duration::from_secs(30)).quiet();
    for (key, value) in envs {
        opts = opts.env(key.clone(), value.clone());
    }
    let _ = runner::run(cmd, opts, |stream, line| report.line(stream, line));
    Ok(())
}

fn run(
    report: &mut Report<'_>,
    cmd: Command,
    envs: &[(String, String)],
    stall: Duration,
) -> Result<runner::RunResult> {
    let mut opts = RunOptions::default().stall(stall);
    for (key, value) in envs {
        opts = opts.env(key.clone(), value.clone());
    }
    runner::run(cmd, opts, |stream, line| report.line(stream, line))
}

fn run_checked(
    report: &mut Report<'_>,
    cmd: Command,
    envs: &[(String, String)],
    stall: Duration,
    what: &str,
) -> Result<()> {
    let result = run(report, cmd, envs, stall)?;
    if !result.success() {
        bail!("{what} failed: {}", result.tail_text(10));
    }
    Ok(())
}

fn latest_version(repo: &str) -> Option<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let text = http::fetch_text(&url).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    Some(strip_tag(tag))
}

fn strip_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('v').to_string()
}

fn backend_name(backend: GpuBackend) -> &'static str {
    match backend {
        GpuBackend::Vkd3d => "vkd3d",
        GpuBackend::Dxvk => "dxvk",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxvk_dll_list_has_seven_entries() {
        assert_eq!(DXVK_DLLS.len(), 7);
        assert!(DXVK_DLLS.contains(&"dxgi.dll"));
    }

    #[test]
    fn strips_release_tag_prefix() {
        assert_eq!(strip_tag("v2.3"), "2.3");
        assert_eq!(strip_tag(" 3.0a "), "3.0a");
    }

    #[test]
    fn dry_run_vkd3d_does_not_create_files() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        let opts = BackendOptions {
            prefix: prefix.clone(),
            backend: GpuBackend::Vkd3d,
            opencl: true,
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        let outcome = install(&opts, |_, line| lines.push(line.to_string())).unwrap();
        assert_eq!(outcome.backend, GpuBackend::Vkd3d);
        let text = lines.join("\n");
        assert!(text.contains("vkd3d-proton-3.0a.tar.zst"));
        assert!(text.contains("d3d12core"));
        assert!(text.contains(".opencl_enabled = 1"));
        assert!(!prefix.join("vkd3d_dlls").exists());
        assert!(!prefix.join(".cache").exists());
    }

    #[test]
    fn dry_run_dxvk_uses_winetricks() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        fs::create_dir_all(&prefix).unwrap();
        let opts = BackendOptions {
            prefix,
            backend: GpuBackend::Dxvk,
            opencl: false,
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        install(&opts, |_, line| lines.push(line.to_string())).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("winetricks --unattended --no-isolate --optout --force dxvk"));
        assert!(text.contains("/v d3d8 "));
        assert!(text.contains(".opencl_enabled = 0"));
    }

    #[test]
    fn launch_target_prefers_hook() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(launch_target(dir.path()), app_exe(dir.path()));
        fs::create_dir_all(app_exe(dir.path()).parent().unwrap()).unwrap();
        fs::write(app_exe(dir.path()), b"exe").unwrap();
        fs::write(
            app_exe(dir.path()).with_file_name("AffinityHook.exe"),
            b"hook",
        )
        .unwrap();
        assert!(launch_target(dir.path()).ends_with("AffinityHook.exe"));
    }
}
