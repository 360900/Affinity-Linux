use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{Config, config_path};
use crate::deps::{self, DepCheck};
use crate::distro::{Family, OsRelease};
use crate::gpu::{self, Gpu};
use crate::manifest::Manifest;
use crate::{WINE_DIR_NAME, app_exe};

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub os: OsRelease,
    pub family: Family,
    pub package_manager: Option<String>,
    pub gpus: Vec<Gpu>,
    pub deps: Vec<DepCheck>,
    pub config_path: PathBuf,
    pub prefix: PathBuf,
    pub prefix_exists: bool,
    pub wine_build: Option<String>,
    pub installed: bool,
    pub manifest: Option<Manifest>,
    pub errors: Vec<String>,
}

pub fn run(cfg: &Config) -> DoctorReport {
    let mut errors = Vec::new();

    let os = OsRelease::detect().unwrap_or_else(|err| {
        errors.push(format!("os-release: {err:#}"));
        OsRelease::default()
    });

    let family = match cfg.distro_override.as_deref() {
        Some(over) => override_family(over),
        None => os.family(),
    };

    let package_manager = {
        let pm = family.package_manager();
        (!pm.is_empty() && deps::which(pm).is_some()).then(|| pm.to_string())
    };

    let prefix = cfg.prefix.clone();
    let prefix_exists = prefix.is_dir();

    let manifest = match Manifest::load(&prefix) {
        Ok(manifest) => manifest,
        Err(err) => {
            errors.push(format!("manifest: {err:#}"));
            None
        }
    };

    let wine_build = fs::read_link(prefix.join(WINE_DIR_NAME))
        .ok()
        .and_then(|target| {
            target
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .or_else(|| {
            prefix
                .join(WINE_DIR_NAME)
                .join("bin/wine")
                .is_file()
                .then(|| WINE_DIR_NAME.to_string())
        });

    let installed = app_exe(&prefix).is_file()
        || manifest
            .as_ref()
            .and_then(|m| m.affinity_exe.as_deref())
            .is_some_and(|path| PathBuf::from(path).is_file());

    DoctorReport {
        os,
        family,
        package_manager,
        gpus: gpu::detect(),
        deps: deps::check_all(),
        config_path: config_path(),
        prefix,
        prefix_exists,
        wine_build,
        installed,
        manifest,
        errors,
    }
}

fn override_family(value: &str) -> Family {
    match value.to_ascii_lowercase().as_str() {
        "arch" | "cachyos" => Family::Arch,
        "ubuntu" => Family::Ubuntu,
        "debian" => Family::Debian,
        "fedora" => Family::Fedora,
        "suse" | "opensuse" => Family::Suse,
        _ => Family::Unknown,
    }
}
