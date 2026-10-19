//! Post-install extras: WinRT facades, PluginLoader, settings seed, WebView2.

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
use crate::{WINE_DIR_NAME, seven_zip_out};

const DOTNET48_URL: &str = "https://download.visualstudio.microsoft.com/download/pr/7afca223-55d2-470a-8edc-6a1739ae3252/abd170b4b0ec15ad0222a809b761a036/ndp48-x86-x64-allos-enu.exe";
const DOTNET48_SHA256: &str = "95889d6de3f2070c07790ad6cf2000d33d9a1bdfc6a381725ab82ab1c314fd53";
const DOTNET48_INSTALLER: &str = "ndp48-x86-x64-allos-enu.exe";
const DOTNET48_CAB: &str = "x64-Windows10.0-KB4486153-x64.cab";
const FACADES: [(&str, &str); 2] = [
    (
        "System.Runtime.WindowsRuntime",
        "1e1e6b4ac4e758fe1066e315f7928c393e1216dbe55c318803dd107123e1a8e3",
    ),
    (
        "System.Runtime.WindowsRuntime.UI.Xaml",
        "8e035a8667213e5a87b69757251a00c7c5f72c5b1b8f7578ec20688629b189e1",
    ),
];

const SETTINGS_ZIP: &str = "https://github.com/seapear/AffinityOnLinux/archive/refs/heads/main.zip";
const PLUGIN_RELEASE_API: &str =
    "https://api.github.com/repos/noahc3/AffinityPluginLoader/releases/latest";
const WEBVIEW2_URL: &str = "https://github.com/ryzendew/AffinityOnLinux/releases/download/10.4-Wine-Affinity/MicrosoftEdgeWebView2RuntimeInstallerX64.exe";

pub fn install_facades<F>(
    prefix: &Path,
    dry_run: bool,
    log_path: Option<PathBuf>,
    on_line: F,
) -> Result<bool>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);
    report.info("WinRT facades for .NET 4.8");

    if facades_installed(prefix) && !dry_run {
        report.info("facades already present");
        return Ok(true);
    }
    if dry_run {
        let installer = dotnet48_cache_path()?;
        report.info(format!(
            "    curl -fL -o {} {DOTNET48_URL}",
            installer.display()
        ));
        report.info(format!(
            "    7z e -y -o<temp> {} {DOTNET48_CAB}",
            installer.display()
        ));
        report.info(
            "    7z e -y -o<temp>/dll <temp>/x64-Windows10.0-KB4486153-x64.cab msil_system.runtime.windowsruntime_b77a5c561934e089_*/*",
        );
        return Ok(true);
    }
    let registry = fs::read_to_string(prefix.join("system.reg")).unwrap_or_default();
    if !crate::wine::has_dotnet48(&registry) {
        report.warn("skipping facades: .NET Framework 4.8 is not registered");
        return Ok(false);
    }
    if which("7z").is_none() {
        report.warn("skipping facades: 7z is missing");
        return Ok(false);
    }

    let installer = dotnet48_cache_path()?;
    if !installer.exists() || http::sha256_file(&installer).unwrap_or_default() != DOTNET48_SHA256 {
        if installer.exists() {
            let _ = fs::remove_file(&installer);
        }
        http::download(
            &mut report,
            DOTNET48_URL,
            &installer,
            Duration::from_secs(300),
        )?;
        if http::sha256_file(&installer)? != DOTNET48_SHA256 {
            let _ = fs::remove_file(&installer);
            bail!("the .NET 4.8 installer failed its SHA-256 check");
        }
    }

    let temp = prefix.join(format!(".winrt-facades-{}", now_epoch()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    let mut cmd = Command::new("7z");
    cmd.args(["e", "-y"])
        .arg(seven_zip_out(&temp))
        .arg(&installer)
        .arg(DOTNET48_CAB);
    let _ = run(&mut report, cmd, &[], Duration::from_secs(600))?;
    let cab = temp.join(DOTNET48_CAB);
    if !cab.exists() {
        let _ = fs::remove_dir_all(&temp);
        report.warn("the .NET 4.8 Windows 10 servicing CAB was not found");
        return Ok(false);
    }

    let dll_dir = temp.join("dll");
    for (name, _) in FACADES {
        let pattern = format!("msil_{}_b77a5c561934e089_*/*", name.to_ascii_lowercase());
        let mut cmd = Command::new("7z");
        cmd.args(["e", "-y"])
            .arg(seven_zip_out(&dll_dir))
            .arg(&cab)
            .arg(pattern);
        let _ = run(&mut report, cmd, &[], Duration::from_secs(600))?;
    }

    for (name, expected) in FACADES {
        let source = dll_dir.join(format!("{}.dll", name.to_ascii_lowercase()));
        if !source.exists() {
            let _ = fs::remove_dir_all(&temp);
            report.warn(format!("{name}.dll was not found in the servicing CAB"));
            return Ok(false);
        }
        if http::sha256_file(&source)? != expected {
            let _ = fs::remove_dir_all(&temp);
            report.warn(format!("unexpected checksum for {name}.dll"));
            return Ok(false);
        }
        let target = gac_path(prefix, name);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(&source, &target)?;
    }

    let _ = fs::remove_dir_all(&temp);
    report.info("WinRT facades installed into the GAC");
    Ok(true)
}

pub fn install_plugin_loader<F>(
    prefix: &Path,
    dry_run: bool,
    log_path: Option<PathBuf>,
    on_line: F,
) -> Result<bool>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);
    let app_dir = prefix.join("drive_c/Program Files/Affinity/Affinity");
    report.info("AffinityPluginLoader + WineFix");
    if !app_dir.is_dir() && !dry_run {
        report.warn("skipping PluginLoader: Affinity is not installed");
        return Ok(false);
    }

    if dry_run {
        report.info(format!("    curl -fsSL {PLUGIN_RELEASE_API}"));
        report.info("    download affinitypluginloader*.zip and winefix*.zip, extract into the app directory");
        return Ok(true);
    }

    let release = http::fetch_text(PLUGIN_RELEASE_API)
        .context("fetching the AffinityPluginLoader release")?;
    let json: serde_json::Value = serde_json::from_str(&release)?;
    let assets = json
        .get("assets")
        .and_then(|value| value.as_array())
        .context("release metadata has no assets")?;
    let find = |prefix: &str| -> Option<(String, String)> {
        assets.iter().find_map(|asset| {
            let name = asset.get("name")?.as_str()?.to_string();
            if name.to_ascii_lowercase().starts_with(prefix) && name.ends_with(".zip") {
                let url = asset.get("browser_download_url")?.as_str()?.to_string();
                Some((name, url))
            } else {
                None
            }
        })
    };
    let loader = find("affinitypluginloader").context("no AffinityPluginLoader zip asset")?;
    let winefix = find("winefix").context("no WineFix zip asset")?;

    let temp = prefix.join(format!(".pluginloader-{}", now_epoch()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    for (name, url) in [loader, winefix] {
        let zip = temp.join(&name);
        http::download(&mut report, &url, &zip, Duration::from_secs(300))?;
        let out = temp.join(format!("{name}-extracted"));
        let mut cmd = Command::new("7z");
        cmd.args(["x", "-y"]).arg(seven_zip_out(&out)).arg(&zip);
        let result = run(&mut report, cmd, &[], Duration::from_secs(300))?;
        if !result.success() {
            report.warn(format!("could not extract {name}"));
            continue;
        }
        copy_tree(&out, &app_dir)?;
    }

    let hook = app_dir.join("AffinityHook.exe");
    if !hook.exists() {
        let _ = fs::remove_dir_all(&temp);
        report.warn("AffinityHook.exe was not installed");
        return Ok(false);
    }

    // APL needs no first run: launching AffinityHook.exe is all it asks for,
    // and the launcher and desktop entry already do that.
    let _ = fs::remove_dir_all(&temp);
    report.info("PluginLoader + WineFix installed");
    Ok(true)
}

pub fn seed_settings<F>(
    prefix: &Path,
    dry_run: bool,
    log_path: Option<PathBuf>,
    on_line: F,
) -> Result<bool>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);
    let user = windows_user(prefix);
    let target = prefix
        .join("drive_c/users")
        .join(&user)
        .join("AppData/Roaming/Affinity/Affinity/3.0/Settings");
    report.info(format!("Affinity settings seed for user {user}"));

    if dry_run {
        report.info(format!(
            "    curl -fL -o <temp>/settings.zip {SETTINGS_ZIP} && 7z x -y -o<temp> <temp>/settings.zip"
        ));
        report.info(format!(
            "    cp -a <temp>/AffinityOnLinux-main/Auxiliary/Settings/Affinity/3.0/Settings {}",
            target.display()
        ));
        return Ok(true);
    }

    let temp = prefix.join(format!(".temp-settings-{}", now_epoch()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    let zip = temp.join("settings.zip");
    http::download(&mut report, SETTINGS_ZIP, &zip, Duration::from_secs(300))?;
    let mut cmd = Command::new("7z");
    cmd.args(["x", "-y"]).arg(seven_zip_out(&temp)).arg(&zip);
    let result = run(&mut report, cmd, &[], Duration::from_secs(300))?;
    if !result.success() {
        let _ = fs::remove_dir_all(&temp);
        bail!("could not extract the settings archive");
    }

    let source = fs::read_dir(&temp)?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_dir()
                && path
                    .join("Auxiliary/Settings/Affinity/3.0/Settings")
                    .is_dir()
        })
        .map(|path| path.join("Auxiliary/Settings/Affinity/3.0/Settings"));
    let Some(source) = source else {
        let _ = fs::remove_dir_all(&temp);
        bail!("the settings archive did not contain Auxiliary/Settings/Affinity/3.0/Settings");
    };

    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::create_dir_all(target.parent().unwrap())?;
    copy_tree(&source, &target)?;
    let _ = fs::remove_dir_all(&temp);
    report.info(format!("settings copied to {}", target.display()));
    Ok(true)
}

pub fn install_webview2<F>(
    prefix: &Path,
    dry_run: bool,
    log_path: Option<PathBuf>,
    on_line: F,
) -> Result<bool>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);
    report.info("Microsoft Edge WebView2 Runtime");

    let installed = webview2_installed(prefix);
    if dry_run {
        if installed {
            report.info("WebView2 is already installed");
        } else {
            report.info(format!(
                "    curl -fL -o <prefix>/WebView2Installer.exe {WEBVIEW2_URL}"
            ));
            report.info(format!(
                "    WINEPREFIX={} {}/bin/wine <prefix>/WebView2Installer.exe",
                prefix.display(),
                prefix.join(WINE_DIR_NAME).display()
            ));
        }
        report.info("    wine reg add HKLM\\System\\CurrentControlSet\\Services\\edgeupdate /v Start /d 4 /f");
        report.info("    wine reg add HKCU\\Software\\Wine\\AppDefaults\\msedgewebview2.exe /v Version /d win11 /f");
        return Ok(true);
    }

    if !installed {
        let installer = prefix.join("WebView2Installer.exe");
        if which("wine").is_none() {
            report.warn("skipping WebView2: wine is missing");
            return Ok(false);
        }
        http::download(
            &mut report,
            WEBVIEW2_URL,
            &installer,
            Duration::from_secs(300),
        )?;
        let wine = prefix.join(WINE_DIR_NAME).join("bin/wine");
        let envs = wine_env(prefix);
        let mut cmd = Command::new(&wine);
        cmd.arg(&installer);
        let _ = run(&mut report, cmd, &envs, Duration::from_secs(1800));
        let _ = fs::remove_file(&installer);
    }

    apply_webview2_registry(prefix, &mut report)?;
    if webview2_installed(prefix) {
        report.info("WebView2 installed and configured");
        Ok(true)
    } else {
        report.warn("WebView2 was not found after the installer finished");
        Ok(false)
    }
}

fn apply_webview2_registry(prefix: &Path, report: &mut Report<'_>) -> Result<()> {
    let wine = prefix.join(WINE_DIR_NAME).join("bin/wine");
    let envs = wine_env(prefix);
    let entries = [
        (
            "HKLM\\System\\CurrentControlSet\\Services\\edgeupdate",
            "Start",
            "4",
        ),
        (
            "HKLM\\System\\CurrentControlSet\\Services\\edgeupdatem",
            "Start",
            "4",
        ),
        (
            "HKCU\\Software\\Wine\\AppDefaults\\msedgewebview2.exe",
            "Version",
            "win11",
        ),
    ];
    for (key, name, value) in entries {
        let mut cmd = Command::new(&wine);
        cmd.args(["reg", "add", key, "/v", name, "/d", value, "/f"]);
        let _ = run(report, cmd, &envs, Duration::from_secs(60));
    }
    Ok(())
}

/// The runtime lives in a versioned folder, e.g.
/// `EdgeWebView/Application/142.0.3595.94/msedgewebview2.exe`.
fn webview2_installed(prefix: &Path) -> bool {
    ["Program Files (x86)", "Program Files"]
        .iter()
        .map(|programs| {
            prefix
                .join("drive_c")
                .join(programs)
                .join("Microsoft/EdgeWebView/Application")
        })
        .filter_map(|application| fs::read_dir(application).ok())
        .flatten()
        .flatten()
        .any(|entry| entry.path().join("msedgewebview2.exe").is_file())
}

fn facades_installed(prefix: &Path) -> bool {
    FACADES
        .iter()
        .all(|(name, _)| gac_path(prefix, name).exists())
}

fn gac_path(prefix: &Path, name: &str) -> PathBuf {
    prefix
        .join("drive_c/windows/Microsoft.NET/assembly/GAC_MSIL")
        .join(name)
        .join("v4.0_4.0.0.0__b77a5c561934e089")
        .join(format!("{name}.dll"))
}

fn dotnet48_cache_path() -> Result<PathBuf> {
    let cache = match std::env::var_os("XDG_CACHE_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(".cache")
        }
    };
    Ok(cache.join("winetricks/dotnet48").join(DOTNET48_INSTALLER))
}

fn windows_user(prefix: &Path) -> String {
    let users = prefix.join("drive_c/users");
    fs::read_dir(&users)
        .map(|entries| {
            let mut names: Vec<String> = entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                .filter(|name| {
                    !matches!(
                        name.as_str(),
                        "Public" | "Default" | "All Users" | "Default User"
                    )
                })
                .collect();
            names.sort();
            names
                .into_iter()
                .next()
                .unwrap_or_else(|| "Public".to_string())
        })
        .unwrap_or_else(|_| "Public".to_string())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn wine_env(prefix: &Path) -> Vec<(String, String)> {
    let wine_dir = prefix.join(WINE_DIR_NAME);
    let path = std::env::var("PATH").unwrap_or_default();
    vec![
        ("WINEPREFIX".into(), prefix.display().to_string()),
        ("WINEDEBUG".into(), "-all,fixme-all".into()),
        ("WINESERVER_TIMEOUT".into(), "60".into()),
        (
            "PATH".into(),
            format!("{}:{path}", wine_dir.join("bin").display()),
        ),
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_paths_match_the_gac_layout() {
        let path = gac_path(Path::new("/p"), "System.Runtime.WindowsRuntime");
        assert!(path.ends_with(
            "GAC_MSIL/System.Runtime.WindowsRuntime/v4.0_4.0.0.0__b77a5c561934e089/System.Runtime.WindowsRuntime.dll"
        ));
    }

    #[test]
    fn webview2_detection_looks_both_program_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!webview2_installed(dir.path()));
        let application = dir
            .path()
            .join("drive_c/Program Files (x86)/Microsoft/EdgeWebView/Application");
        fs::create_dir_all(application.join("SetupMetrics")).unwrap();
        assert!(!webview2_installed(dir.path()));
        let exe = application.join("142.0.3595.94/msedgewebview2.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(exe, b"exe").unwrap();
        assert!(webview2_installed(dir.path()));
    }

    #[test]
    fn windows_user_skips_system_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("drive_c/users");
        for name in ["Public", "Default", "tarik"] {
            fs::create_dir_all(users.join(name)).unwrap();
        }
        assert_eq!(windows_user(dir.path()), "tarik");
    }

    #[test]
    fn dry_runs_have_no_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = Vec::new();
        install_webview2(dir.path(), true, None, |_, line| {
            lines.push(line.to_string());
        })
        .unwrap();
        let text = lines.join("\n");
        assert!(text.contains("MicrosoftEdgeWebView2RuntimeInstallerX64.exe"));
        assert!(!dir.path().join("WebView2Installer.exe").exists());

        assert!(install_plugin_loader(dir.path(), true, None, |_, _| {}).unwrap());
        assert!(seed_settings(dir.path(), true, None, |_, _| {}).unwrap());
    }
}
