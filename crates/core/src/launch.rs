//! Launch Affinity with the Wine environment and guards from the specs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::backends;
use crate::config;
use crate::gpu::{self, Vendor};
use crate::journal::Report;
use crate::manifest::now_epoch;
use crate::runner::{self, RunOptions, Stream};
use crate::{WINE_DIR_NAME, app_exe};

pub const CORRUPTION_SIGNATURE: &str = "JPEG XL: input does not have a valid signature";

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub prefix: PathBuf,
    pub args: Vec<String>,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    pub target: PathBuf,
    pub quarantined: Option<PathBuf>,
}

pub fn launch<F>(opts: &LaunchOptions, on_line: F) -> Result<LaunchOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let wine_dir = opts.prefix.join(WINE_DIR_NAME);
    let wine = wine_dir.join("bin/wine");

    if !opts.dry_run && !app_exe(&opts.prefix).exists() {
        bail!(
            "Affinity is not installed ({} is missing)",
            app_exe(&opts.prefix).display()
        );
    }
    if !opts.dry_run && !wine.exists() {
        bail!(
            "Wine is not installed at {}; run wine-setup first",
            wine.display()
        );
    }

    let mut quarantined = None;
    if !opts.dry_run
        && let Some(path) = guard_corruption(&opts.prefix)?
    {
        report.warn(format!(
            "Affinity profile had the JPEG XL corruption signature; moved to {}",
            path.display()
        ));
        quarantined = Some(path);
    }

    if !opts.dry_run && backends::sync_app_dlls(&opts.prefix)? {
        report.info("synced the vkd3d-proton runtime next to Affinity");
    }

    let envs = launch_env(&opts.prefix, &wine_dir);
    force_x11_if_wayland(&mut report, &wine, &envs, opts.dry_run)?;

    let target = backends::launch_target(&opts.prefix);
    let mut cmd = Command::new(&wine);
    cmd.arg(&target);
    cmd.args(&opts.args);
    for (key, value) in &envs {
        cmd.env(key, value);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    if opts.dry_run {
        report.info(format!(
            "would run {} {} {}",
            wine.display(),
            target.display(),
            opts.args.join(" ")
        ));
        for (key, value) in &envs {
            report.info(format!("env {key}={value}"));
        }
        return Ok(LaunchOutcome {
            target,
            quarantined,
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", target.display()))?;
    report.info(format!(
        "launched {} (pid {})",
        target.display(),
        child.id()
    ));
    Ok(LaunchOutcome {
        target,
        quarantined,
    })
}

/// Move a corrupted Affinity v3 profile aside and recreate an empty one.
pub fn guard_corruption(prefix: &Path) -> Result<Option<PathBuf>> {
    let user_data = user_data_dir(prefix);
    let log = user_data.join("Log.txt");
    let corrupt = fs::read_to_string(&log)
        .map(|text| text.contains(CORRUPTION_SIGNATURE))
        .unwrap_or(false);
    if !corrupt || !user_data.exists() {
        return Ok(None);
    }

    let parent = user_data
        .parent()
        .context("Affinity user data has no parent")?;
    let name = user_data
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("3.0");
    let mut backup = parent.join(format!("{name}.backup-{}", now_epoch()));
    let mut suffix = 1;
    while backup.exists() {
        backup = parent.join(format!("{name}.backup-{}-{suffix}", now_epoch()));
        suffix += 1;
    }
    fs::rename(&user_data, &backup)
        .with_context(|| format!("moving {} to {}", user_data.display(), backup.display()))?;
    fs::create_dir_all(&user_data)
        .with_context(|| format!("recreating {}", user_data.display()))?;
    Ok(Some(backup))
}

fn user_data_dir(prefix: &Path) -> PathBuf {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string());
    prefix
        .join("drive_c/users")
        .join(user)
        .join("AppData/Roaming/Affinity/Affinity/3.0")
}

fn force_x11_if_wayland(
    report: &mut Report<'_>,
    wine: &Path,
    envs: &[(String, String)],
    dry_run: bool,
) -> Result<()> {
    if !wayland_session() {
        return Ok(());
    }
    let mut cmd = Command::new(wine);
    cmd.args([
        "reg",
        "add",
        "HKEY_CURRENT_USER\\Software\\Wine\\Drivers",
        "/v",
        "Graphics",
        "/t",
        "REG_SZ",
        "/d",
        "x11",
        "/f",
    ]);
    if dry_run {
        report.info(format!(
            "{} (Wayland session: forcing the Wine X11 driver)",
            describe(&cmd)
        ));
        return Ok(());
    }
    let mut run_opts = RunOptions::default().stall(Duration::from_secs(60)).quiet();
    for (key, value) in envs {
        run_opts = run_opts.env(key.clone(), value.clone());
    }
    match runner::run(cmd, run_opts, |stream, line| report.line(stream, line)) {
        Ok(result) if result.success() => {}
        Ok(result) => report.warn(format!(
            "could not force the Wine X11 driver (exit {})",
            result.status
        )),
        Err(error) => report.warn(format!("could not force the Wine X11 driver: {error}")),
    }
    Ok(())
}

fn launch_env(prefix: &Path, wine_dir: &Path) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    let path = std::env::var("PATH").unwrap_or_default();
    envs.push(("WINEPREFIX".into(), prefix.display().to_string()));
    envs.push(("WINEDEBUG".into(), "-all,fixme-all".into()));
    envs.push(("PATH".into(), format!("{}/bin:{path}", wine_dir.display())));
    envs.push((
        "WINE".into(),
        wine_dir.join("bin/wine").display().to_string(),
    ));
    if wine_dir.join("bin/wineserver").exists() {
        envs.push((
            "WINESERVER".into(),
            wine_dir.join("bin/wineserver").display().to_string(),
        ));
    }
    envs.push((
        "DISPLAY".into(),
        std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string()),
    ));
    if let Ok(xauthority) = std::env::var("XAUTHORITY") {
        envs.push(("XAUTHORITY".into(), xauthority));
    }

    let mut overrides = vec!["d3d12=n,b", "d3d12core=n,b"];
    if app_exe(prefix)
        .parent()
        .map(|dir| dir.join("mscms.dll").exists())
        .unwrap_or(false)
    {
        overrides.push("mscms=n,b");
    }
    envs.push(("WINEDLLOVERRIDES".into(), overrides.join(";")));

    let selection =
        config::read_prefix_state(prefix, ".gpu_config").unwrap_or_else(|| "auto".into());
    envs.extend(gpu_env(&selection));
    if let Some((device, value)) = vulkan_device_select(&selection) {
        envs.push((device, value));
    }

    for (key, value) in vulkan_runtime_env(wayland_session(), has_nvidia()) {
        envs.push((key.to_string(), value.to_string()));
    }

    envs
}

fn gpu_env(selection: &str) -> Vec<(String, String)> {
    if selection.is_empty() || selection == "auto" {
        return Vec::new();
    }
    if selection.starts_with("nvidia_") {
        return vec![
            ("__NV_PRIME_RENDER_OFFLOAD".into(), "1".into()),
            ("__GLX_VENDOR_LIBRARY_NAME".into(), "nvidia".into()),
            ("__VK_LAYER_NV_optimus".into(), "NVIDIA_only".into()),
        ];
    }
    if let Some(index) = selection.strip_prefix("amd_") {
        return vec![("DRI_PRIME".into(), index.to_string())];
    }
    if selection.starts_with("intel_") {
        return vec![("DRI_PRIME".into(), "0".into())];
    }
    Vec::new()
}

fn vulkan_device_select(selection: &str) -> Option<(String, String)> {
    let (kind, raw_index) = selection.split_once('_')?;
    if !matches!(kind, "nvidia" | "amd" | "intel") {
        return None;
    }
    let wanted: usize = raw_index.parse().ok()?;
    let output = Command::new("lspci").arg("-nn").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            (lower.contains("vga")
                || lower.contains("3d controller")
                || lower.contains("display controller"))
                && match kind {
                    "nvidia" => lower.contains("nvidia"),
                    "amd" => {
                        lower.contains("amd")
                            || lower.contains("radeon")
                            || lower.contains("advanced micro devices")
                    }
                    "intel" => lower.contains("intel"),
                    _ => false,
                }
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    if wanted >= lines.len() {
        lines.truncate(1);
    }
    let line = lines.get(wanted).or_else(|| lines.first())?;
    let id = pci_id(line)?;
    Some(("MESA_VK_DEVICE_SELECT".into(), id))
}

fn pci_id(line: &str) -> Option<String> {
    let start = line.rfind('[')?;
    let end = line[start..].find(']')? + start;
    let inner = &line[start + 1..end];
    let (vendor, device) = inner.split_once(':')?;
    if vendor.len() == 4
        && device.len() == 4
        && vendor.chars().all(|c| c.is_ascii_hexdigit())
        && device.chars().all(|c| c.is_ascii_hexdigit())
    {
        Some(format!(
            "{}:{}",
            vendor.to_ascii_lowercase(),
            device.to_ascii_lowercase()
        ))
    } else {
        None
    }
}

fn vulkan_runtime_env(wayland: bool, nvidia: bool) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![
        ("DXVK_ASYNC", "0"),
        (
            "DXVK_CONFIG",
            "d3d9.deferSurfaceCreation = True; d3d9.shaderModel = 1",
        ),
        ("DXVK_LOG_LEVEL", "none"),
        ("VKD3D_DEBUG", "none"),
        ("VKD3D_FEATURE_LEVEL", "12_1"),
        ("VKD3D_SHADER_DEBUG", "none"),
        ("VKD3D_SHADER_MODEL", "6_5"),
    ];
    if wayland {
        env.push((
            "VKD3D_DISABLE_EXTENSIONS",
            "VK_KHR_present_id,VK_KHR_present_wait",
        ));
        if nvidia {
            env.push(("VKD3D_CONFIG", "swapchain_legacy"));
        }
    } else {
        env.push(("VKD3D_DISABLE_EXTENSIONS", "VK_KHR_present_id"));
    }
    env
}

fn wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|value| value.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false)
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn has_nvidia() -> bool {
    gpu::detect().iter().any(|gpu| gpu.vendor == Vendor::Nvidia)
}

fn describe(cmd: &Command) -> String {
    let mut parts = vec![cmd.get_program().to_string_lossy().to_string()];
    parts.extend(cmd.get_args().map(|arg| arg.to_string_lossy().to_string()));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_moves_profile_and_recreates_it() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let user_data = user_data_dir(prefix);
        fs::create_dir_all(&user_data).unwrap();
        fs::write(user_data.join("Log.txt"), CORRUPTION_SIGNATURE).unwrap();
        fs::write(user_data.join("Settings.xml"), b"old").unwrap();

        let backup = guard_corruption(prefix).unwrap().expect("quarantine");
        assert!(backup.exists());
        assert!(backup.join("Settings.xml").exists());
        assert!(user_data.exists());
        assert!(!user_data.join("Log.txt").exists());
        assert!(guard_corruption(prefix).unwrap().is_none());
    }

    #[test]
    fn gpu_env_for_selected_devices() {
        let nvidia = gpu_env("nvidia_0");
        assert!(
            nvidia
                .iter()
                .any(|(key, _)| key == "__NV_PRIME_RENDER_OFFLOAD")
        );
        let amd = gpu_env("amd_2");
        assert_eq!(amd, vec![("DRI_PRIME".into(), "2".into())]);
        assert!(gpu_env("auto").is_empty());
    }

    #[test]
    fn parses_pci_ids() {
        assert_eq!(
            pci_id("01:00.0 VGA compatible controller: NVIDIA [10de:2484] (rev a1)"),
            Some("10de:2484".into())
        );
        assert_eq!(pci_id("no id here"), None);
    }

    #[test]
    fn vulkan_env_wayland_nvidia() {
        let env = vulkan_runtime_env(true, true);
        assert!(
            env.iter()
                .any(|(key, value)| *key == "VKD3D_CONFIG" && *value == "swapchain_legacy")
        );
        assert!(
            env.iter()
                .any(|(key, value)| *key == "VKD3D_DISABLE_EXTENSIONS"
                    && value.contains("VK_KHR_present_wait"))
        );

        let x11 = vulkan_runtime_env(false, true);
        assert!(!x11.iter().any(|(key, _)| *key == "VKD3D_CONFIG"));
    }

    #[test]
    fn dry_run_does_not_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let wine = prefix.join(WINE_DIR_NAME).join("bin/wine");
        fs::create_dir_all(wine.parent().unwrap()).unwrap();
        fs::write(&wine, b"wine").unwrap();
        fs::create_dir_all(app_exe(prefix).parent().unwrap()).unwrap();
        fs::write(app_exe(prefix), b"exe").unwrap();

        let opts = LaunchOptions {
            prefix: prefix.to_path_buf(),
            args: vec!["--help".into()],
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        let outcome = launch(&opts, |_, line| lines.push(line.to_string())).unwrap();
        assert!(outcome.target.ends_with("Affinity.exe"));
        let text = lines.join("\n");
        assert!(text.contains("would run"));
        assert!(text.contains("env WINEPREFIX="));
        assert!(text.contains("d3d12=n,b"));
        assert!(text.contains("VKD3D_FEATURE_LEVEL=12_1"));
    }
}
