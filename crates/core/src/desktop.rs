//! Application menu entry, affinity:// handler and protocol registry fix.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::deps::which;
use crate::journal::Report;
use crate::runner::{self, RunOptions, Stream};
use crate::{APP_NAME, WINE_DIR_NAME, wine};

pub const URL_HANDLER_FILE: &str = "affinity-url-handler.desktop";

const GAC_FACADES: [&str; 2] = [
    "System.Runtime.WindowsRuntime",
    "System.Runtime.WindowsRuntime.UI.Xaml",
];

#[derive(Debug, Clone)]
pub struct DesktopOptions {
    pub prefix: PathBuf,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct DesktopOutcome {
    pub desktop: PathBuf,
    pub url_handler: bool,
}

pub fn create<F>(opts: &DesktopOptions, on_line: F) -> Result<DesktopOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let applications = applications_dir()?;
    let desktop = applications.join("Affinity.desktop");
    let exec = exec_line(&opts.prefix);
    let contents = desktop_contents(&opts.prefix, &exec);

    report.info(format!("desktop entry: {}", desktop.display()));
    if opts.dry_run {
        report.info(format!("    {}", contents.replace('\n', "\n    ")));
    } else {
        fs::create_dir_all(&applications)?;
        fs::write(&desktop, &contents).with_context(|| format!("writing {}", desktop.display()))?;
        if let Some(desktop_folder) = desktop_dir().filter(|dir| dir.is_dir()) {
            let shortcut = desktop_folder.join("Affinity.desktop");
            if fs::copy(&desktop, &shortcut).is_ok() {
                let _ = fs::set_permissions(&shortcut, fs::Permissions::from_mode(0o755));
            }
        }
    }

    fix_protocol_registry(opts, &mut report)?;

    let mut url_handler = false;
    if can_register_url_handler(&opts.prefix) {
        url_handler = register_url_handler(&applications, &exec, opts.dry_run, &mut report)?;
    } else {
        report.info("affinity:// handler skipped until facades and WinMetadata are present");
    }

    Ok(DesktopOutcome {
        desktop,
        url_handler,
    })
}

fn desktop_contents(prefix: &Path, exec: &str) -> String {
    let icon = "applications-graphics";
    let path_value = escape_desktop_string(&prefix.display().to_string());
    format!(
        "[Desktop Entry]\n\
         Name=Affinity Suite\n\
         Comment=Affinity v3 creative suite\n\
         Icon={icon}\n\
         Path={path_value}\n\
         Exec={exec}\n\
         Terminal=false\n\
         Type=Application\n\
         Categories=Graphics;\n\
         StartupNotify=true\n\
         StartupWMClass=affinity.exe\n"
    )
}

/// Starts Affinity through our own launcher, so menu launches get the same
/// environment (DLL overrides, GPU selection, guards) as the Launch button.
/// Falls back to plain Wine when the launcher binary cannot be found.
fn exec_line(prefix: &Path) -> String {
    match launcher() {
        Some(cli) => launcher_exec(&cli, prefix),
        None => wine_exec_line(prefix),
    }
}

fn launcher() -> Option<PathBuf> {
    let current = std::env::current_exe().ok();
    // AppRun forwards any arguments to the CLI. Only trust an AppImage that is
    // this project: other AppImages in the environment set APPIMAGE too.
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let path = PathBuf::from(appimage);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let mounted = current
            .as_ref()
            .map(|exe| exe.to_string_lossy().contains(".mount_"))
            .unwrap_or(false);
        if name.contains("affinity_linux") || name.contains("affinity-linux") || mounted {
            return Some(path);
        }
    }
    if let Some(current) = current {
        let cli = current.with_file_name(APP_NAME);
        if cli.is_file() {
            return Some(cli);
        }
        if current
            .file_name()
            .map(|name| name == APP_NAME)
            .unwrap_or(false)
        {
            return Some(current);
        }
    }
    which(APP_NAME)
}

fn launcher_exec(cli: &Path, prefix: &Path) -> String {
    format!(
        "{} launch --prefix {}",
        quote(&cli.display().to_string()),
        quote(&prefix.display().to_string())
    )
}

fn wine_exec_line(prefix: &Path) -> String {
    let wine = prefix.join(WINE_DIR_NAME).join("bin/wine");
    let app_dir = prefix.join("drive_c/Program Files/Affinity/Affinity");
    let hook = app_dir.join("AffinityHook.exe");
    let exe = if hook.exists() {
        hook
    } else {
        app_dir.join("Affinity.exe")
    };
    format!(
        "env WINEPREFIX={} {} {}",
        quote(&prefix.display().to_string()),
        quote(&wine.display().to_string()),
        quote(&exe.display().to_string())
    )
}

fn register_url_handler(
    applications: &Path,
    exec: &str,
    dry_run: bool,
    report: &mut Report<'_>,
) -> Result<bool> {
    let handler = applications.join(URL_HANDLER_FILE);
    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Affinity sign-in handler\n\
         Exec={exec} %u\n\
         MimeType=x-scheme-handler/affinity;\n\
         NoDisplay=true\n\
         Terminal=false\n"
    );
    report.info(format!("affinity:// handler: {}", handler.display()));
    if dry_run {
        report.info(format!(
            "    xdg-mime default {URL_HANDLER_FILE} x-scheme-handler/affinity"
        ));
        return Ok(true);
    }

    fs::create_dir_all(applications)?;
    fs::write(&handler, contents).with_context(|| format!("writing {}", handler.display()))?;

    if which("update-desktop-database").is_some() {
        let mut cmd = Command::new("update-desktop-database");
        cmd.arg(applications);
        let _ = runner::run(
            cmd,
            RunOptions::default().stall(Duration::from_secs(60)),
            |stream, line| report.line(stream, line),
        );
    }
    let mut registered = false;
    if which("xdg-mime").is_some() {
        let mut cmd = Command::new("xdg-mime");
        cmd.args(["default", URL_HANDLER_FILE, "x-scheme-handler/affinity"]);
        match runner::run(
            cmd,
            RunOptions::default().stall(Duration::from_secs(60)),
            |stream, line| report.line(stream, line),
        ) {
            Ok(result) if result.success() => registered = true,
            Ok(result) => report.warn(format!(
                "xdg-mime default {URL_HANDLER_FILE} failed ({})",
                result.status
            )),
            Err(error) => report.warn(format!("xdg-mime default failed: {error}")),
        }
    } else {
        report.warn("xdg-mime is missing; cannot make the affinity:// handler the default");
    }

    let wine_entry = applications.join("wine-protocol-affinity.desktop");
    if wine_entry.symlink_metadata().is_ok() {
        let _ = fs::remove_file(wine_entry);
    }
    Ok(registered)
}

fn fix_protocol_registry(opts: &DesktopOptions, report: &mut Report<'_>) -> Result<()> {
    if !opts
        .prefix
        .join("drive_c/Program Files/Affinity/Affinity/Affinity.exe")
        .exists()
        && !opts.dry_run
    {
        return Ok(());
    }
    let wine_dir = opts.prefix.join(WINE_DIR_NAME);
    let wine_bin = wine_dir.join("bin/wine");
    let target = "\"C:\\Program Files\\Affinity\\Affinity\\Affinity.exe\" \"%1\"";
    let mut cmd = Command::new(&wine_bin);
    cmd.args([
        "reg",
        "add",
        "HKEY_CLASSES_ROOT\\affinity\\shell\\open\\command",
        "/ve",
        "/d",
        target,
        "/f",
    ]);
    if opts.dry_run {
        report.info(format!(
            "    WINEPREFIX={} {} reg add HKEY_CLASSES_ROOT\\affinity\\shell\\open\\command /ve /d {} /f",
            opts.prefix.display(),
            wine_bin.display(),
            quote(target)
        ));
        return Ok(());
    }

    let mut options = RunOptions::default().stall(Duration::from_secs(120));
    options.envs = wine::wine_env(&opts.prefix, &wine_dir, "-all");
    let result = runner::run(cmd, options, |stream, line| report.line(stream, line))?;
    if !result.success() {
        report.warn("could not point HKCR\\affinity at the v3 executable");
    }
    Ok(())
}

fn can_register_url_handler(prefix: &Path) -> bool {
    let gac = prefix.join("drive_c/windows/Microsoft.NET/assembly/GAC_MSIL");
    let facades = GAC_FACADES.iter().all(|name| {
        gac.join(name)
            .join("v4.0_4.0.0.0__b77a5c561934e089")
            .join(format!("{name}.dll"))
            .exists()
    });
    let winmetadata = ["winmetadata", "WinMetadata"].iter().any(|name| {
        fs::read_dir(prefix.join("drive_c/windows/system32").join(name))
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("winmd"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });
    facades && winmetadata
}

fn applications_dir() -> Result<PathBuf> {
    Ok(data_home()?.join("applications"))
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> Result<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Ok(home_dir()?.join(".local/share")),
    }
}

/// Resolve the user's desktop directory via XDG user-dirs, falling back to `~/Desktop`.
pub(crate) fn desktop_dir() -> Option<PathBuf> {
    if which("xdg-user-dir").is_some()
        && let Ok(output) = Command::new("xdg-user-dir").arg("DESKTOP").output()
        && output.status.success()
    {
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !value.is_empty() {
            let path = PathBuf::from(&value);
            if home_dir().ok().as_deref() != Some(path.as_path()) {
                return Some(path);
            }
        }
    }
    let home = home_dir().ok()?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    if let Ok(text) = fs::read_to_string(config_home.join("user-dirs.dirs"))
        && let Some(dir) = parse_user_dir(&text, "XDG_DESKTOP_DIR", &home)
    {
        return Some(dir);
    }
    Some(home.join("Desktop"))
}

fn parse_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let prefix = format!("{key}=");
    for line in text.lines() {
        let line = line.trim();
        let Some(raw) = line.strip_prefix(&prefix) else {
            continue;
        };
        let value = raw.trim().trim_matches('"');
        if value.is_empty() {
            return None;
        }
        let path = if let Some(rest) = value.strip_prefix("$HOME/") {
            home.join(rest)
        } else if value == "$HOME" {
            return None;
        } else {
            PathBuf::from(value)
        };
        return Some(path);
    }
    None
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

/// Escape a `.desktop` string value (general string escaping).
fn escape_desktop_string(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Quote a single argument for a `.desktop` `Exec=` line per the Freedesktop
/// Desktop Entry Specification.
pub fn quote(text: &str) -> String {
    let escaped_percent = text.replace('%', "%%");
    if !escaped_percent.is_empty()
        && escaped_percent
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+%".contains(c))
    {
        return escaped_percent;
    }
    let mut out = String::with_capacity(escaped_percent.len() + 2);
    out.push('"');
    for ch in escaped_percent.chars() {
        match ch {
            '\\' => out.push_str("\\\\\\\\"),
            '"' => out.push_str("\\\\\""),
            '`' => out.push_str("\\\\`"),
            '$' => out.push_str("\\\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_is_preferred_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("drive_c/Program Files/Affinity/Affinity");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("Affinity.exe"), b"exe").unwrap();
        assert!(wine_exec_line(dir.path()).contains("Affinity.exe"));
        assert!(!wine_exec_line(dir.path()).contains("AffinityHook.exe"));

        fs::write(app.join("AffinityHook.exe"), b"hook").unwrap();
        assert!(wine_exec_line(dir.path()).contains("AffinityHook.exe"));
    }

    #[test]
    fn quote_escapes_desktop_exec_characters() {
        assert_eq!(quote("/home/me/100%"), "/home/me/100%%");
        assert_eq!(
            quote("/home/me/My $Dir \"a\"\\b"),
            "\"/home/me/My \\\\$Dir \\\\\"a\\\\\"\\\\\\\\b\""
        );
    }

    #[test]
    fn parses_localized_desktop_dir() {
        let home = Path::new("/home/me");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n";
        assert_eq!(
            parse_user_dir(text, "XDG_DESKTOP_DIR", home),
            Some(PathBuf::from("/home/me/Schreibtisch"))
        );
    }

    #[test]
    fn handler_requires_facades_and_winmetadata() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!can_register_url_handler(dir.path()));
        let gac = dir
            .path()
            .join("drive_c/windows/Microsoft.NET/assembly/GAC_MSIL");
        for name in GAC_FACADES {
            let folder = gac.join(name).join("v4.0_4.0.0.0__b77a5c561934e089");
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(format!("{name}.dll")), b"dll").unwrap();
        }
        assert!(!can_register_url_handler(dir.path()));
        fs::create_dir_all(dir.path().join("drive_c/windows/system32/winmetadata")).unwrap();
        fs::write(
            dir.path()
                .join("drive_c/windows/system32/winmetadata/windows.foundation.winmd"),
            b"md",
        )
        .unwrap();
        assert!(can_register_url_handler(dir.path()));
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let options = DesktopOptions {
            prefix: dir.path().to_path_buf(),
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        create(&options, |_, line| lines.push(line.to_string())).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("Exec="));
        assert!(text.contains("HKEY_CLASSES_ROOT\\affinity"));
        assert!(!dir.path().join("Affinity.desktop").exists());
    }

    #[test]
    fn exec_goes_through_the_launcher() {
        let exec = launcher_exec(
            Path::new("/usr/bin/affinity-linux"),
            Path::new("/home/me/My Prefix"),
        );
        assert_eq!(
            exec,
            "/usr/bin/affinity-linux launch --prefix \"/home/me/My Prefix\""
        );
    }
}
