//! Wayland compositors take a window's icon from the desktop file named after
//! its app ID. Packages install one; the AppImage and source builds do not, so
//! the GUI registers a hidden one itself and removes it once a package does.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use affinity_linux_core::deps::which;
use affinity_linux_core::desktop::data_home;
use affinity_linux_core::desktop::quote;

pub const APP_ID: &str = "io.github._360900.AffinityLinux";
const ICON_NAME: &str = "affinity-linux";
const ICON_SVG: &str = include_str!("../../../packaging/affinity-linux.svg");
const DESKTOP: &str = include_str!("../../../packaging/io.github._360900.AffinityLinux.desktop");
/// Marks files written here, so a desktop file the user installed stays.
const MARKER: &str = "X-AffinityLinux-Generated=true";

pub fn register() {
    let Ok(data) = data_home() else {
        return;
    };
    let desktop = data.join("applications").join(format!("{APP_ID}.desktop"));
    let icon = data
        .join("icons/hicolor/scalable/apps")
        .join(format!("{ICON_NAME}.svg"));
    let ours = fs::read_to_string(&desktop).is_ok_and(|text| is_generated(&text));

    if packaged(&system_data_dirs()) {
        // A user-level copy would shadow the packaged entry and icon.
        if ours {
            let _ = fs::remove_file(&desktop);
            let _ = fs::remove_file(&icon);
            refresh();
        }
        return;
    }
    if desktop.exists() && !ours {
        return;
    }
    let Some(exe) = executable() else {
        return;
    };
    let changed =
        write_if_changed(&desktop, &generated_entry(&exe)) | write_if_changed(&icon, ICON_SVG);
    if changed {
        refresh();
    }
}

fn is_generated(text: &str) -> bool {
    text.lines().any(|line| line.trim() == MARKER)
}

/// The packaged entry pointed at this binary, hidden from the menu.
fn generated_entry(exe: &Path) -> String {
    let mut out = String::new();
    for line in DESKTOP.lines() {
        if line.starts_with("Exec=") {
            out.push_str(&format!("Exec={}\n", quote(&exe.display().to_string())));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("NoDisplay=true\n");
    out.push_str(MARKER);
    out.push('\n');
    out
}

/// The AppImage itself rather than its temporary mount, else this binary.
fn executable() -> Option<PathBuf> {
    env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .or_else(|| env::current_exe().ok())
}

/// `$XDG_DATA_DIRS`, without the AppImage mount, which compositors never see.
fn system_data_dirs() -> Vec<PathBuf> {
    let value = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    let mount = env::var_os("APPDIR").map(PathBuf::from);
    value
        .split(':')
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .filter(|dir| !mount.as_ref().is_some_and(|mount| dir.starts_with(mount)))
        .collect()
}

fn packaged(dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        dir.join("applications")
            .join(format!("{APP_ID}.desktop"))
            .is_file()
    })
}

fn write_if_changed(path: &Path, contents: &str) -> bool {
    if fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return false;
    }
    path.parent()
        .is_some_and(|dir| fs::create_dir_all(dir).is_ok())
        && fs::write(path, contents).is_ok()
}

/// KDE reads desktop files from a cache that otherwise updates with a delay,
/// after the window has already picked its icon.
fn refresh() {
    if which("kbuildsycoca6").is_some() {
        let _ = Command::new("kbuildsycoca6")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_entry_is_hidden_and_marked() {
        let text = generated_entry(Path::new("/opt/My Apps/Affinity.AppImage"));
        assert!(text.contains("Exec=\"/opt/My Apps/Affinity.AppImage\"\n"));
        assert!(text.contains("Icon=affinity-linux\n"));
        assert!(text.contains("NoDisplay=true\n"));
        assert!(is_generated(&text));
        assert!(!is_generated(DESKTOP));
        assert_eq!(text.matches("Exec=").count(), 1);
    }

    #[test]
    fn finds_a_packaged_entry() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        assert!(!packaged(&dirs));
        let applications = dir.path().join("applications");
        fs::create_dir_all(&applications).unwrap();
        fs::write(applications.join(format!("{APP_ID}.desktop")), DESKTOP).unwrap();
        assert!(packaged(&dirs));
    }

    #[test]
    fn writes_only_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.desktop");
        assert!(write_if_changed(&path, "x"));
        assert!(!write_if_changed(&path, "x"));
        assert!(write_if_changed(&path, "y"));
    }
}
