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
    /// PCI address with domain, e.g. "0000:01:00.0".
    pub slot: String,
    /// "vendor:device" in lowercase hex, e.g. "10de:2484".
    pub pci_id: Option<String>,
    pub discrete: bool,
}

impl Gpu {
    /// Mesa's DRI_PRIME form of the PCI address, e.g. "pci-0000_01_00_0".
    pub fn dri_prime_tag(&self) -> String {
        format!("pci-{}", self.slot.replace([':', '.'], "_"))
    }
}

/// Detect display controllers via lspci. Returns an empty list if lspci is absent.
pub fn detect() -> Vec<Gpu> {
    let Ok(output) = Command::new("lspci").arg("-Dnn").output() else {
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
        let Some((head, rest)) = line.split_once(": ") else {
            continue;
        };
        let name = strip_device_id(rest.trim());
        if name.is_empty() {
            continue;
        }
        let slot = head.split_whitespace().next().unwrap_or_default();
        let slot = if slot.matches(':').count() == 1 {
            format!("0000:{slot}")
        } else {
            slot.to_string()
        };
        let vendor = classify(&name);
        gpus.push(Gpu {
            discrete: is_discrete(vendor, &name, &slot),
            pci_id: pci_id(rest),
            vendor,
            name,
            slot,
        });
    }
    gpus
}

/// The GPU to render on when the user has not picked one: the first discrete
/// GPU, but only when there is more than one GPU to choose from.
pub fn preferred(gpus: &[Gpu]) -> Option<&Gpu> {
    if gpus.len() < 2 {
        return None;
    }
    gpus.iter().find(|gpu| gpu.discrete)
}

fn is_discrete(vendor: Vendor, name: &str, slot: &str) -> bool {
    match vendor {
        Vendor::Nvidia => true,
        // Intel iGPUs always sit at 00:02.0; Arc cards live behind a bridge.
        Vendor::Intel => !slot.ends_with(":00:02.0"),
        // APUs show up as codenames ("Rembrandt [Radeon 680M]"), dGPUs as
        // Navi chips or RX/Pro products.
        Vendor::Amd => [
            "Navi",
            "Radeon RX",
            "Radeon Pro",
            "Radeon VII",
            "FirePro",
            "Instinct",
        ]
        .iter()
        .any(|marker| name.contains(marker)),
        Vendor::Other => false,
    }
}

fn pci_id(line: &str) -> Option<String> {
    let start = line.rfind('[')?;
    let end = line[start..].find(']')? + start;
    let inner = &line[start + 1..end];
    let (vendor, device) = inner.split_once(':')?;
    if vendor.len() == 4
        && device.len() == 4
        && vendor.chars().all(|c| c.is_ascii_hexdigit())
        && device.chars().all(|c| c.is_ascii_hexdigit())
    {
        Some(format!(
            "{}:{}",
            vendor.to_ascii_lowercase(),
            device.to_ascii_lowercase()
        ))
    } else {
        None
    }
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
    fn reads_slot_and_pci_id() {
        let text = "0000:01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA104 [GeForce RTX 3070] [10de:2484] (rev a1)\n";
        let gpu = &parse(text)[0];
        assert_eq!(gpu.slot, "0000:01:00.0");
        assert_eq!(gpu.pci_id.as_deref(), Some("10de:2484"));
        assert_eq!(gpu.dri_prime_tag(), "pci-0000_01_00_0");

        let text =
            "01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA104 [10de:2484]\n";
        assert_eq!(parse(text)[0].slot, "0000:01:00.0");
    }

    #[test]
    fn prefers_discrete_on_hybrid_systems() {
        let laptop = parse(
            "0000:00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [Iris Xe Graphics] [8086:a7a0] (rev 04)\n\
             0000:01:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD103GLM [RTX 5000 Ada Generation Laptop GPU] [10de:2730] (rev a1)\n",
        );
        assert!(!laptop[0].discrete);
        assert_eq!(preferred(&laptop).unwrap().vendor, Vendor::Nvidia);

        let amd = parse(
            "0000:03:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Navi 23 [Radeon RX 6600M] [1002:73ff] (rev c3)\n\
             0000:07:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Rembrandt [Radeon 680M] [1002:1681] (rev c8)\n",
        );
        assert!(!amd[1].discrete);
        assert_eq!(preferred(&amd).unwrap().slot, "0000:03:00.0");

        let arc = parse(
            "0000:00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-S GT1 [UHD Graphics 770] [8086:a780] (rev 04)\n\
             0000:03:00.0 VGA compatible controller [0300]: Intel Corporation DG2 [Arc A770] [8086:56a0] (rev 08)\n",
        );
        assert_eq!(preferred(&arc).unwrap().slot, "0000:03:00.0");

        assert!(preferred(&laptop[1..]).is_none());
    }

    #[test]
    fn ignores_non_display_lines() {
        let text = "00:1f.3 Audio device [0403]: Intel Corporation Raptor Lake-P HD Audio Controller [8086:51ca]\n";
        assert!(parse(text).is_empty());
    }
}
