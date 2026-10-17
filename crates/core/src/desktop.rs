//! Application menu entry, affinity:// handler and protocol registry fix.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::WINE_DIR_NAME;
use crate::deps::which;
use crate::journal::Report;
use crate::runner::{self, RunOptions, Stream};

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
        let shortcut = home_dir()?.join("Desktop").join("Affinity.desktop");
        if shortcut
            .parent()
            .map(|parent| parent.is_dir())
            .unwrap_or(false)
        {
            let _ = fs::copy(&desktop, &shortcut);
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
    format!(
        "[Desktop Entry]\n\
         Name=Affinity Suite\n\
         Comment=Affinity v3 creative suite\n\
         Icon={icon}\n\
         Path={}\n\
         Exec={exec}\n\
         Terminal=false\n\
         Type=Application\n\
         Categories=Graphics;\n\
         StartupNotify=true\n\
         StartupWMClass=affinity.exe\n",
        prefix.display()
    )
}

fn exec_line(prefix: &Path) -> String {
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
    if which("xdg-mime").is_some() {
        let mut cmd = Command::new("xdg-mime");
        cmd.args(["default", URL_HANDLER_FILE, "x-scheme-handler/affinity"]);
        let _ = runner::run(
            cmd,
            RunOptions::default().stall(Duration::from_secs(60)),
            |stream, line| report.line(stream, line),
        );
    } else {
        report.warn("xdg-mime is missing; cannot make the affinity:// handler the default");
    }

    let wine_entry = applications.join("wine-protocol-affinity.desktop");
    if wine_entry.exists() {
        let _ = fs::remove_file(wine_entry);
    }
    Ok(true)
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
    let wine = opts.prefix.join(WINE_DIR_NAME).join("bin/wine");
    let target = "\"C:\\Program Files\\Affinity\\Affinity\\Affinity.exe\" \"%1\"";
    let mut cmd = Command::new(&wine);
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
            wine.display(),
            quote(target)
        ));
        return Ok(());
    }

    let mut options = RunOptions::default().stall(Duration::from_secs(120));
    options = options.env("WINEPREFIX", opts.prefix.display().to_string());
    options = options.env("WINEDEBUG", "-all");
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
    let data = match std::env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home_dir()?.join(".local/share"),
    };
    Ok(data.join("applications"))
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn quote(text: &str) -> String {
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+".contains(c))
    {
        text.to_string()
    } else {
        format!("\"{}\"", text.replace('"', "\\\""))
    }
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
        assert!(exec_line(dir.path()).contains("Affinity.exe"));
        assert!(!exec_line(dir.path()).contains("AffinityHook.exe"));

        fs::write(app.join("AffinityHook.exe"), b"hook").unwrap();
        assert!(exec_line(dir.path()).contains("AffinityHook.exe"));
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
        assert!(text.contains("Exec=env WINEPREFIX="));
        assert!(text.contains("HKEY_CLASSES_ROOT\\affinity"));
        assert!(!dir.path().join("Affinity.desktop").exists());
    }
}
