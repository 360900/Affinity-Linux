use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    #[default]
    Vkd3d,
    Dxvk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub prefix: PathBuf,
    pub gpu_backend: GpuBackend,
    pub opencl: bool,
    /// Override distro detection, mainly for dry-run testing.
    pub distro_override: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: default_prefix(),
            gpu_backend: GpuBackend::default(),
            opencl: false,
            distro_override: None,
        }
    }
}

impl Config {
    /// Load the app config, falling back to defaults when the file is absent.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Self =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.prefix = expand_home(&cfg.prefix);
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, text).with_context(|| format!("writing config {}", path.display()))
    }
}

pub fn config_dir() -> PathBuf {
    match env::var_os("XDG_CONFIG_HOME") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join(crate::APP_NAME),
        _ => home_dir().join(".config").join(crate::APP_NAME),
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn default_prefix() -> PathBuf {
    home_dir().join(crate::PREFIX_DIR_NAME)
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Expand a leading `~` or `~/...` to the user home directory.
pub fn expand_home(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if s == "~" {
        return home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

// Per-prefix state files written by the installer.
pub fn prefix_state_path(prefix: &Path, name: &str) -> PathBuf {
    prefix.join(name)
}

pub fn write_prefix_state(prefix: &Path, name: &str, value: &str) -> Result<()> {
    fs::create_dir_all(prefix)
        .with_context(|| format!("creating prefix dir {}", prefix.display()))?;
    let path = prefix_state_path(prefix, name);
    fs::write(&path, value).with_context(|| format!("writing {}", path.display()))
}

pub fn read_prefix_state(prefix: &Path, name: &str) -> Option<String> {
    fs::read_to_string(prefix_state_path(prefix, name))
        .ok()
        .map(|v| v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config {
            prefix: PathBuf::from("/tmp/prefix"),
            gpu_backend: GpuBackend::Dxvk,
            opencl: true,
            distro_override: Some("ubuntu".into()),
        };
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.prefix, PathBuf::from("/tmp/prefix"));
        assert_eq!(loaded.gpu_backend, GpuBackend::Dxvk);
        assert!(loaded.opencl);
        assert_eq!(loaded.distro_override.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn missing_file_uses_defaults() {
        let cfg = Config::load_from(Path::new("/nonexistent/affinity/config.toml")).unwrap();
        assert!(cfg.prefix.ends_with(crate::PREFIX_DIR_NAME));
    }

    #[test]
    fn expands_tilde() {
        let home = home_dir();
        assert_eq!(expand_home(Path::new("~")), home);
        assert_eq!(expand_home(Path::new("~/x")), home.join("x"));
        assert_eq!(expand_home(Path::new("/abs")), PathBuf::from("/abs"));
    }

    #[test]
    fn prefix_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_prefix_state(dir.path(), ".opencl_enabled", "1").unwrap();
        assert_eq!(
            read_prefix_state(dir.path(), ".opencl_enabled").as_deref(),
            Some("1")
        );
        assert_eq!(read_prefix_state(dir.path(), ".missing"), None);
    }
}
