//! Launch Affinity with the Wine environment and guards from the specs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::backends;
use crate::config;
use crate::gpu::{self, Gpu, Vendor};
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
    /// Experimental: use Wine's native Wayland driver instead of XWayland.
    pub wayland: bool,
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

    let gpus = gpu::detect();
    let selection =
        config::read_prefix_state(&opts.prefix, ".gpu_config").unwrap_or_else(|| "auto".into());
    let chosen = select_gpu(&gpus, &selection);
    if let Some(gpu) = chosen {
        report.info(format!("rendering on {} ({})", gpu.name, gpu.slot));
    }
    let nvidia = gpus.iter().any(|gpu| gpu.vendor == Vendor::Nvidia);
    let envs = launch_env(&opts.prefix, &wine_dir, chosen, nvidia);
    if wayland_session() {
        let driver = if opts.wayland {
            report.warn("using Wine's experimental Wayland driver");
            "wayland"
        } else {
            "x11"
        };
        set_graphics_driver(&mut report, &wine, &envs, driver, opts.dry_run)?;
    } else if opts.wayland {
        report.warn("not a Wayland session; ignoring --wayland");
    }

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

/// Pins Wine's graphics driver in the prefix. Written on every Wayland
/// launch, so a normal launch after `--wayland` goes back to X11.
fn set_graphics_driver(
    report: &mut Report<'_>,
    wine: &Path,
    envs: &[(String, String)],
    driver: &str,
    dry_run: bool,
) -> Result<()> {
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
        driver,
        "/f",
    ]);
    if dry_run {
        report.info(format!(
            "{} (Wayland session: using the Wine {driver} driver)",
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
            "could not set the Wine {driver} driver (exit {})",
            result.status
        )),
        Err(error) => report.warn(format!("could not set the Wine {driver} driver: {error}")),
    }
    Ok(())
}

fn launch_env(
    prefix: &Path,
    wine_dir: &Path,
    gpu: Option<&Gpu>,
    nvidia: bool,
) -> Vec<(String, String)> {
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

    if let Some(gpu) = gpu {
        envs.extend(gpu_env(gpu));
    }

    for (key, value) in vulkan_runtime_env(wayland_session(), nvidia) {
        envs.push((key.to_string(), value.to_string()));
    }

    envs
}

/// Resolves `.gpu_config`: "auto" picks the discrete GPU on multi-GPU systems,
/// "<vendor>_<n>" picks the n-th GPU of that vendor.
fn select_gpu<'a>(gpus: &'a [Gpu], selection: &str) -> Option<&'a Gpu> {
    if selection.is_empty() || selection == "auto" {
        return gpu::preferred(gpus);
    }
    let (kind, raw_index) = selection.split_once('_')?;
    let vendor = match kind {
        "nvidia" => Vendor::Nvidia,
        "amd" => Vendor::Amd,
        "intel" => Vendor::Intel,
        _ => return None,
    };
    let matching: Vec<&Gpu> = gpus.iter().filter(|gpu| gpu.vendor == vendor).collect();
    let wanted: usize = raw_index.parse().unwrap_or(0);
    matching.get(wanted).or_else(|| matching.first()).copied()
}

fn gpu_env(gpu: &Gpu) -> Vec<(String, String)> {
    let mut envs: Vec<(String, String)> = if gpu.vendor == Vendor::Nvidia {
        vec![
            ("__NV_PRIME_RENDER_OFFLOAD".into(), "1".into()),
            ("__GLX_VENDOR_LIBRARY_NAME".into(), "nvidia".into()),
            ("__VK_LAYER_NV_optimus".into(), "NVIDIA_only".into()),
        ]
    } else {
        vec![("DRI_PRIME".into(), gpu.dri_prime_tag())]
    };
    if let Some(id) = &gpu.pci_id {
        envs.push(("MESA_VK_DEVICE_SELECT".into(), id.clone()));
    }
    envs
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

pub(crate) fn wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE")
        .map(|value| value.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false)
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
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

    fn hybrid() -> Vec<Gpu> {
        gpu::parse(
            "0000:00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [Iris Xe Graphics] [8086:a7a0] (rev 04)\n\
             0000:01:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD103GLM [RTX 5000 Ada Generation Laptop GPU] [10de:2730] (rev a1)\n\
             0000:03:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Navi 23 [Radeon RX 6600M] [1002:73ff] (rev c3)\n",
        )
    }

    #[test]
    fn auto_selects_discrete_gpu() {
        let gpus = hybrid();
        let chosen = select_gpu(&gpus, "auto").unwrap();
        assert_eq!(chosen.vendor, Vendor::Nvidia);
        let env = gpu_env(chosen);
        assert!(env.contains(&("__NV_PRIME_RENDER_OFFLOAD".into(), "1".into())));
        assert!(env.contains(&("MESA_VK_DEVICE_SELECT".into(), "10de:2730".into())));

        assert!(select_gpu(&gpus[..1], "auto").is_none());
    }

    #[test]
    fn explicit_selection_overrides_auto() {
        let gpus = hybrid();
        let amd = select_gpu(&gpus, "amd_0").unwrap();
        assert_eq!(
            gpu_env(amd),
            vec![
                ("DRI_PRIME".into(), "pci-0000_03_00_0".into()),
                ("MESA_VK_DEVICE_SELECT".into(), "1002:73ff".into()),
            ]
        );
        assert_eq!(select_gpu(&gpus, "intel_0").unwrap().slot, "0000:00:02.0");
        assert!(select_gpu(&gpus, "bogus").is_none());
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
            wayland: false,
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
