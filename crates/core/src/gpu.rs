use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl Vendor {
    pub fn label(&self) -> &'static str {
        match self {
            Vendor::Nvidia => "nvidia",
            Vendor::Amd => "amd",
            Vendor::Intel => "intel",
            Vendor::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Gpu {
    pub vendor: Vendor,
    pub name: String,
}

/// Detect display controllers via lspci. Returns an empty list if lspci is absent.
pub fn detect() -> Vec<Gpu> {
    let Ok(output) = Command::new("lspci").arg("-nn").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

pub fn parse(text: &str) -> Vec<Gpu> {
    let mut gpus = Vec::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        let is_display = lower.contains("vga compatible controller")
            || lower.contains("3d controller")
            || lower.contains("display controller");
        if !is_display {
            continue;
        }
        // The first ": " comes after the class name, e.g.
        // 01:00.0 VGA compatible controller [0300]: NVIDIA Corporation ... [10de:2484]
        let Some((_, rest)) = line.split_once(": ") else {
            continue;
        };
        let name = strip_device_id(rest.trim());
        if name.is_empty() {
            continue;
        }
        gpus.push(Gpu {
            vendor: classify(&name),
            name,
        });
    }
    gpus
}

fn strip_device_id(name: &str) -> String {
    let mut name = name.trim();
    // lspci appends "(rev xx)" after the vendor/device ids.
    if name.ends_with(')')
        && let Some(idx) = name.rfind(" (rev ")
    {
        name = name[..idx].trim();
    }
    if name.ends_with(']')
        && let Some(idx) = name.rfind(" [")
    {
        let inner = &name[idx + 2..name.len() - 1];
        if inner.len() == 9 && inner.as_bytes().get(4) == Some(&b':') {
            name = name[..idx].trim();
        }
    }
    name.to_string()
}

fn classify(name: &str) -> Vendor {
    let lower = name.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.contains(&"nvidia") {
        Vendor::Nvidia
    } else if tokens.contains(&"amd")
        || tokens.contains(&"ati")
        || lower.contains("advanced micro devices")
    {
        Vendor::Amd
    } else if tokens.contains(&"intel") {
        Vendor::Intel
    } else {
        Vendor::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_line() {
        let text = "01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA104 [GeForce RTX 3070] [10de:2484] (rev a1)\n";
        let gpus = parse(text);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vendor, Vendor::Nvidia);
        assert_eq!(gpus[0].name, "NVIDIA Corporation GA104 [GeForce RTX 3070]");
    }

    #[test]
    fn parses_amd_line() {
        let text = "0c:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Navi 22 [Radeon RX 6700/6700 XT/6750 XT/6800M] [1002:73df] (rev c1)\n";
        let gpus = parse(text);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vendor, Vendor::Amd);
    }

    #[test]
    fn parses_intel_line() {
        let text = "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [Iris Xe Graphics] [8086:a7a0] (rev 04)\n";
        let gpus = parse(text);
        assert_eq!(gpus[0].vendor, Vendor::Intel);
        assert_eq!(
            gpus[0].name,
            "Intel Corporation Raptor Lake-P [Iris Xe Graphics]"
        );
    }

    #[test]
    fn ignores_non_display_lines() {
        let text = "00:1f.3 Audio device [0403]: Intel Corporation Raptor Lake-P HD Audio Controller [8086:51ca]\n";
        assert!(parse(text).is_empty());
    }
}
