use std::env;
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DepCheck {
    pub name: &'static str,
    pub command: &'static str,
    pub required: bool,
    pub found: bool,
    pub version: Option<String>,
}

struct Spec {
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    required: bool,
}

const SPECS: &[Spec] = &[
    Spec {
        name: "wine",
        command: "wine",
        args: &["--version"],
        required: true,
    },
    Spec {
        name: "wineserver",
        command: "wineserver",
        args: &["--version"],
        required: true,
    },
    Spec {
        name: "winetricks",
        command: "winetricks",
        args: &["--version"],
        required: true,
    },
    Spec {
        name: "wget",
        command: "wget",
        args: &["--version"],
        required: false,
    },
    Spec {
        name: "curl",
        command: "curl",
        args: &["--version"],
        required: false,
    },
    Spec {
        name: "7z",
        command: "7z",
        args: &["i"],
        required: true,
    },
    Spec {
        name: "unzip",
        command: "unzip",
        args: &["-v"],
        required: false,
    },
    Spec {
        name: "tar",
        command: "tar",
        args: &["--version"],
        required: true,
    },
    Spec {
        name: "jq",
        command: "jq",
        args: &["--version"],
        required: false,
    },
    Spec {
        name: "zstd",
        command: "zstd",
        args: &["--version"],
        required: true,
    },
    Spec {
        name: "dotnet",
        command: "dotnet",
        args: &["--list-sdks"],
        required: true,
    },
    Spec {
        name: "lspci",
        command: "lspci",
        args: &["--version"],
        required: true,
    },
];

pub fn check_all() -> Vec<DepCheck> {
    SPECS.iter().map(check).collect()
}

fn check(spec: &Spec) -> DepCheck {
    let result = Command::new(spec.command).args(spec.args).output();
    let (found, version) = match result {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.trim().is_empty() {
                text = String::from_utf8_lossy(&output.stderr).to_string();
            }
            let version = text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.chars().take(100).collect::<String>());
            let version = match version {
                Some(version) => Some(version),
                // dotnet present but no SDK installed (runtime only).
                None if spec.command == "dotnet" => Some("no SDK found".to_string()),
                None => None,
            };
            (true, version)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (false, None),
        Err(err) => (true, Some(format!("error: {err}"))),
    };
    DepCheck {
        name: spec.name,
        command: spec.command,
        required: spec.required,
        found,
        version,
    }
}

pub fn missing(checks: &[DepCheck]) -> Vec<&DepCheck> {
    checks.iter().filter(|c| c.required && !c.found).collect()
}

/// Locate a command on PATH without executing it.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}
