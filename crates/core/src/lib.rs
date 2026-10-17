//! Headless engine for the Affinity v3 Linux installer.

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
pub mod manifest;
pub mod packages;
pub mod patch;
pub mod runner;
pub mod wine;

use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "affinity-linux";
pub const PREFIX_DIR_NAME: &str = ".AffinityLinux";
pub const WINE_DIR_NAME: &str = "ElementalWarriorWine";
pub const APP_EXE_REL: &str = "drive_c/Program Files/Affinity/Affinity/Affinity.exe";

/// Absolute path of the installed Affinity v3 executable inside a prefix.
pub fn app_exe(prefix: &Path) -> PathBuf {
    prefix.join(APP_EXE_REL)
}
