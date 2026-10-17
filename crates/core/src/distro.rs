use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct OsRelease {
    pub id: String,
    pub id_like: Vec<String>,
    pub version_id: Option<String>,
    pub pretty_name: Option<String>,
}

impl OsRelease {
    /// Read and parse `/etc/os-release` (or the lib fallback).
    pub fn detect() -> Result<Self> {
        for path in ["/etc/os-release", "/usr/lib/os-release"] {
            if Path::new(path).is_file() {
                let text = fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
                return Ok(Self::parse(&text));
            }
        }
        anyhow::bail!("no os-release file found")
    }

    pub fn parse(text: &str) -> Self {
        let mut os = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = unquote(value.trim());
            match key {
                "ID" => os.id = value.to_ascii_lowercase(),
                "ID_LIKE" => {
                    os.id_like = value
                        .split_whitespace()
                        .map(|v| v.to_ascii_lowercase())
                        .collect()
                }
                "VERSION_ID" => os.version_id = Some(value),
                "PRETTY_NAME" => os.pretty_name = Some(value),
                _ => {}
            }
        }
        os
    }

    pub fn family(&self) -> Family {
        let id = self.id.to_ascii_lowercase();
        let like: Vec<String> = self
            .id_like
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        let matched = |names: &[&str]| {
            names
                .iter()
                .any(|n| id == *n || like.iter().any(|l| l == n))
        };

        if matched(&[
            "arch",
            "archlinux",
            "cachyos",
            "endeavouros",
            "manjaro",
            "garuda",
        ]) {
            Family::Arch
        } else if matched(&[
            "pika",
            "pikaos",
            "ubuntu",
            "linuxmint",
            "mint",
            "pop",
            "zorin",
            "elementary",
        ]) {
            Family::Ubuntu
        } else if matched(&["debian"]) {
            Family::Debian
        } else if matched(&["fedora", "rhel", "centos", "nobara"]) {
            Family::Fedora
        } else if id.starts_with("opensuse") || matched(&["opensuse", "suse"]) {
            Family::Suse
        } else {
            Family::Unknown
        }
    }

    pub fn name(&self) -> String {
        self.pretty_name.clone().unwrap_or_else(|| self.id.clone())
    }

    pub fn version_major(&self) -> Option<u32> {
        self.version_id
            .as_deref()
            .and_then(|v| v.split('.').next())
            .and_then(|v| v.parse().ok())
    }
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Arch,
    Ubuntu,
    Debian,
    Fedora,
    Suse,
    Unknown,
}

impl Family {
    pub fn label(&self) -> &'static str {
        match self {
            Family::Arch => "arch",
            Family::Ubuntu => "ubuntu",
            Family::Debian => "debian",
            Family::Fedora => "fedora",
            Family::Suse => "suse",
            Family::Unknown => "unknown",
        }
    }

    pub fn package_manager(&self) -> &'static str {
        match self {
            Family::Arch => "pacman",
            Family::Ubuntu | Family::Debian => "apt-get",
            Family::Fedora => "dnf",
            Family::Suse => "zypper",
            Family::Unknown => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family_of(text: &str) -> Family {
        OsRelease::parse(text).family()
    }

    #[test]
    fn parses_quoting_and_lists() {
        let os = OsRelease::parse(
            "ID=ubuntu\nID_LIKE=\"debian\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04 LTS\"\n",
        );
        assert_eq!(os.id, "ubuntu");
        assert_eq!(os.id_like, vec!["debian"]);
        assert_eq!(os.version_id.as_deref(), Some("24.04"));
        assert_eq!(os.name(), "Ubuntu 24.04 LTS");
        assert_eq!(os.version_major(), Some(24));
    }

    #[test]
    fn arch_family() {
        assert_eq!(family_of("ID=cachyos\nID_LIKE=arch\n"), Family::Arch);
        assert_eq!(family_of("ID=arch\n"), Family::Arch);
        assert_eq!(family_of("ID=endeavouros\nID_LIKE=arch\n"), Family::Arch);
    }

    #[test]
    fn ubuntu_family() {
        assert_eq!(family_of("ID=ubuntu\nID_LIKE=debian\n"), Family::Ubuntu);
        assert_eq!(
            family_of("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
            Family::Ubuntu
        );
        assert_eq!(
            family_of("ID=pop\nID_LIKE=\"ubuntu debian\"\n"),
            Family::Ubuntu
        );
        assert_eq!(family_of("ID=zorin\nID_LIKE=ubuntu\n"), Family::Ubuntu);
        assert_eq!(
            family_of("ID=pika\nID_LIKE=\"ubuntu debian\"\n"),
            Family::Ubuntu
        );
        assert_eq!(family_of("ID=pikaos\n"), Family::Ubuntu);
    }

    #[test]
    fn other_families() {
        assert_eq!(family_of("ID=debian\n"), Family::Debian);
        assert_eq!(family_of("ID=fedora\n"), Family::Fedora);
        assert_eq!(
            family_of("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n"),
            Family::Suse
        );
        assert_eq!(family_of("ID=void\n"), Family::Unknown);
    }

    #[test]
    fn package_managers() {
        assert_eq!(Family::Arch.package_manager(), "pacman");
        assert_eq!(Family::Ubuntu.package_manager(), "apt-get");
        assert_eq!(Family::Fedora.package_manager(), "dnf");
        assert_eq!(Family::Suse.package_manager(), "zypper");
    }
}
