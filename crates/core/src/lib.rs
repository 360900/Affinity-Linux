//! Headless engine for the Affinity v3 Linux installer.

pub mod backends;
pub mod config;
pub mod deps;
pub mod desktop;
pub mod distro;
pub mod doctor;
pub mod extras;
pub mod gpu;
pub mod http;
pub mod install;
pub mod installer;
mod journal;
pub mod launch;
pub mod manifest;
pub mod packages;
pub mod patch;
pub mod runner;
pub mod uninstall;
pub mod wine;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "affinity-linux";
pub const PREFIX_DIR_NAME: &str = ".AffinityLinux";
pub const WINE_DIR_NAME: &str = "ElementalWarriorWine";
pub const APP_EXE_REL: &str = "drive_c/Program Files/Affinity/Affinity/Affinity.exe";

/// 7z's output switch. It must be one argument: `-o <dir>` is rejected as
/// "Too short switch".
pub(crate) fn seven_zip_out(dir: &Path) -> OsString {
    let mut arg = OsString::from("-o");
    arg.push(dir);
    arg
}

/// Absolute path of the installed Affinity v3 executable inside a prefix.
pub fn app_exe(prefix: &Path) -> PathBuf {
    prefix.join(APP_EXE_REL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_zip_out_is_a_single_argument() {
        assert_eq!(seven_zip_out(Path::new("/tmp/out")), "-o/tmp/out");
    }
}
