use std::path::Path;
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
    pub fn brand(&self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA",
            Vendor::Amd => "AMD",
            Vendor::Intel => "Intel",
            Vendor::Other => "this GPU",
        }
    }

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

/// Resolves `.gpu_config`: "auto" picks the discrete GPU on multi-GPU systems,
/// "<vendor>_<n>" picks the n-th GPU of that vendor.
pub fn select<'a>(gpus: &'a [Gpu], selection: &str) -> Option<&'a Gpu> {
    if selection.is_empty() || selection == "auto" {
        return preferred(gpus);
    }
    let (kind, raw_index) = selection.split_once('_')?;
    let vendor = match kind {
        "nvidia" => Vendor::Nvidia,
        "amd" => Vendor::Amd,
        "intel" => Vendor::Intel,
        _ => return None,
    };
    let matching: Vec<&Gpu> = gpus.iter().filter(|gpu| gpu.vendor == vendor).collect();
    let wanted: usize = raw_index.parse().unwrap_or(0);
    matching.get(wanted).or_else(|| matching.first()).copied()
}

/// The GPU Affinity renders on: the selected one, else the first one.
pub fn render_gpu<'a>(gpus: &'a [Gpu], selection: &str) -> Option<&'a Gpu> {
    select(gpus, selection).or_else(|| gpus.first())
}

/// Vendor of the GPU Affinity renders on, using the prefix's `.gpu_config`.
pub fn render_vendor(prefix: &Path) -> Option<Vendor> {
    let selection =
        crate::config::read_prefix_state(prefix, ".gpu_config").unwrap_or_else(|| "auto".into());
    render_gpu(&detect(), &selection).map(|gpu| gpu.vendor)
}

/// OpenCL default when the user has not chosen: on for NVIDIA, where it works
/// well under Wine; off for AMD and Intel, where upstream finds it unreliable.
pub fn opencl_default(vendor: Option<Vendor>) -> bool {
    vendor == Some(Vendor::Nvidia)
}

/// True when the host has an OpenCL driver (ICD) registered for `vendor`.
/// Wine's opencl.dll forwards to it.
pub fn opencl_driver_present(vendor: Vendor) -> bool {
    let names: Vec<String> = std::fs::read_dir("/etc/OpenCL/vendors")
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    has_icd_for(vendor, &names)
}

fn has_icd_for(vendor: Vendor, icd_files: &[String]) -> bool {
    // AMD counts only ROCm's ICD: upstream's AMD setup wants ROCm, and Mesa's
    // rusticl/clover alone does not cover it.
    let marker = match vendor {
        Vendor::Nvidia => "nvidia",
        Vendor::Amd => "amdocl",
        Vendor::Intel => "intel",
        Vendor::Other => return false,
    };
    icd_files
        .iter()
        .any(|name| name.ends_with(".icd") && name.contains(marker))
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
    fn opencl_defaults_on_for_nvidia_only() {
        assert!(opencl_default(Some(Vendor::Nvidia)));
        assert!(!opencl_default(Some(Vendor::Amd)));
        assert!(!opencl_default(Some(Vendor::Intel)));
        assert!(!opencl_default(None));
    }

    #[test]
    fn matches_vendor_icd_files() {
        let files = vec!["nvidia.icd".to_string(), "rusticl.icd".to_string()];
        assert!(has_icd_for(Vendor::Nvidia, &files));
        assert!(!has_icd_for(Vendor::Amd, &files));
        assert!(has_icd_for(
            Vendor::Amd,
            &["amdocl64_60402.icd".to_string()]
        ));
        assert!(has_icd_for(Vendor::Intel, &["intel.icd".to_string()]));
        assert!(!has_icd_for(Vendor::Intel, &[]));
    }

    #[test]
    fn render_gpu_is_the_discrete_or_only_gpu() {
        let apu_and_nvidia = parse(
            "0000:01:00.0 VGA compatible controller [0300]: NVIDIA Corporation GA107M [GeForce RTX 3050 Mobile] [10de:25a2] (rev a1)\n\
             0000:05:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Rembrandt [Radeon 680M] [1002:1681] (rev c8)\n",
        );
        assert_eq!(
            render_gpu(&apu_and_nvidia, "auto").unwrap().vendor,
            Vendor::Nvidia
        );
        assert_eq!(
            render_gpu(&apu_and_nvidia, "amd_0").unwrap().vendor,
            Vendor::Amd
        );
        assert_eq!(
            render_gpu(&apu_and_nvidia[1..], "auto").unwrap().vendor,
            Vendor::Amd
        );
        assert!(render_gpu(&[], "auto").is_none());
    }

    #[test]
    fn ignores_non_display_lines() {
        let text = "00:1f.3 Audio device [0403]: Intel Corporation Raptor Lake-P HD Audio Controller [8086:51ca]\n";
        assert!(parse(text).is_empty());
    }
}
