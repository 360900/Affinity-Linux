//! Per-distribution dependency installation plans.
//!
//! The package lists follow the upstream documentation for each supported
//! distribution family. Commands are rendered as shell snippets so they can be
//! shown in a dry run, logged, and executed with sudo or pkexec.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::distro::{Family, OsRelease};
use crate::runner::{self, RunOptions, Stream};

const WINEHQ_KEY: &str = "https://dl.winehq.org/wine-builds/winehq.key";
const WINEHQ_BASE: &str = "https://dl.winehq.org/wine-builds";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageStep {
    pub label: String,
    pub script: String,
    pub needs_root: bool,
    pub optional: bool,
}

impl PackageStep {
    fn root(label: impl Into<String>, script: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            script: script.into(),
            needs_root: true,
            optional: false,
        }
    }

    fn root_optional(label: impl Into<String>, script: impl Into<String>) -> Self {
        Self {
            optional: true,
            ..Self::root(label, script)
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackagePlan {
    pub family: Family,
    pub flavor: String,
    pub steps: Vec<PackageStep>,
    pub notes: Vec<String>,
}

pub fn plan(os: &OsRelease) -> PackagePlan {
    plan_with_multilib(os, None)
}

fn arch_multilib_enabled() -> bool {
    fs::read_to_string("/etc/pacman.conf")
        .map(|text| text.lines().any(|line| line.trim() == "[multilib]"))
        .unwrap_or(true)
}

fn plan_with_multilib(os: &OsRelease, multilib: Option<bool>) -> PackagePlan {
    let family = os.family();
    let id = os.id.to_ascii_lowercase();
    let mut notes = Vec::new();

    if id == "manjaro" {
        notes.push(
            "Manjaro is not officially supported. The Arch plan may still work, but expect rough edges."
                .to_string(),
        );
    }
    if id.contains("bazzite") {
        notes.push(
            "Bazzite is an immutable distribution; automatic dependency installation is disabled."
                .to_string(),
        );
        return PackagePlan {
            family,
            flavor: id,
            steps: Vec::new(),
            notes,
        };
    }

    let (flavor, steps) = match family {
        Family::Arch => arch_plan(multilib.unwrap_or_else(arch_multilib_enabled)),
        Family::Ubuntu => ubuntu_plan(os),
        Family::Fedora => fedora_plan(),
        Family::Suse => suse_plan(),
        Family::Debian => {
            notes.push(
                "Debian is not officially supported. Install wine, winetricks, wget, curl, 7z, tar, jq, zstd, xz and a .NET SDK 8/10 manually."
                    .to_string(),
            );
            ("debian".to_string(), Vec::new())
        }
        Family::Unknown => {
            notes.push(
                "Unknown distribution. Install wine, winetricks, wget, curl, 7z, tar, jq, zstd, xz and a .NET SDK 8/10 manually."
                    .to_string(),
            );
            ("unknown".to_string(), Vec::new())
        }
    };

    PackagePlan {
        family,
        flavor,
        steps,
        notes,
    }
}

fn arch_plan(multilib_enabled: bool) -> (String, Vec<PackageStep>) {
    let mut steps = Vec::new();
    if !multilib_enabled {
        steps.push(PackageStep::root(
            "Enable the Arch multilib repository",
            "grep -q '^\\[multilib\\]' /etc/pacman.conf || printf '\\n[multilib]\\nInclude = /etc/pacman.d/mirrorlist\\n' >> /etc/pacman.conf",
        ));
    }
    steps.push(PackageStep::root(
        "Install Wine and build dependencies",
        "pacman -Syu --needed --noconfirm wine winetricks wget curl p7zip unzip tar jq zstd xz pciutils",
    ));
    steps.push(PackageStep::root_optional(
        "Install .NET SDKs",
        "pacman -S --needed --noconfirm dotnet-sdk dotnet-sdk-8.0 dotnet-sdk-10.0",
    ));
    ("arch".to_string(), steps)
}

fn ubuntu_plan(os: &OsRelease) -> (String, Vec<PackageStep>) {
    let id = os.id.to_ascii_lowercase();
    let (major, minor) = version_tuple(os);

    match id.as_str() {
        "pika" | "pikaos" => {
            let mut steps = Vec::new();
            add_winehq_repo(&mut steps, "debian/dists/forky/winehq-forky.sources");
            steps.push(PackageStep::root(
                "Install WineHQ staging and dependencies",
                "apt-get install --install-recommends -y winehq-staging wget curl p7zip-full tar jq zstd xz-utils pciutils unzip git make",
            ));
            steps.push(PackageStep::root(
                "Build winetricks from source",
                "rm -rf /tmp/affinity-winetricks && git clone --depth=1 https://github.com/Winetricks/winetricks /tmp/affinity-winetricks && make -C /tmp/affinity-winetricks install && rm -rf /tmp/affinity-winetricks",
            ));
            steps.push(dotnet_step(
                "apt-get install -y dotnet-sdk-8.0 dotnet-sdk-10.0",
            ));
            ("pikaos-winehq-forky".to_string(), steps)
        }
        "linuxmint" | "mint" => {
            let (codename, sources) = if major >= 22 {
                ("noble", "ubuntu/dists/noble/winehq-noble.sources")
            } else {
                ("jammy", "ubuntu/dists/jammy/winehq-jammy.sources")
            };
            let mut steps = Vec::new();
            add_winehq_repo(&mut steps, sources);
            steps.push(winehq_install_step(true));
            steps.push(dotnet_step(
                "apt-get install -y dotnet-sdk-8.0 dotnet-sdk-10.0",
            ));
            (format!("mint-{major}-winehq-{codename}"), steps)
        }
        "zorin" => {
            let (codename, sources) = if major >= 18 {
                ("noble", "ubuntu/dists/noble/winehq-noble.sources")
            } else {
                ("jammy", "ubuntu/dists/jammy/winehq-jammy.sources")
            };
            let mut steps = Vec::new();
            add_winehq_repo(&mut steps, sources);
            steps.push(winehq_install_step(true));
            steps.push(dotnet_step(
                "apt-get install -y dotnet-sdk-8.0 dotnet-sdk-10.0",
            ));
            (format!("zorin-{major}-winehq-{codename}"), steps)
        }
        "pop" => {
            let mut steps = Vec::new();
            add_winehq_repo(&mut steps, "ubuntu/dists/jammy/winehq-jammy.sources");
            steps.push(winehq_install_step(true));
            steps.push(dotnet_step("apt-get install -y dotnet-sdk-8.0"));
            ("pop-winehq-jammy".to_string(), steps)
        }
        _ => {
            if (major, minor) >= (24, 4) {
                ubuntu_official_plan(major, minor)
            } else {
                let mut steps = Vec::new();
                add_winehq_repo(&mut steps, "ubuntu/dists/jammy/winehq-jammy.sources");
                steps.push(winehq_install_step(true));
                steps.push(dotnet_step(
                    "apt-get install -y dotnet-sdk-8.0 dotnet-sdk-10.0",
                ));
                ("ubuntu-winehq-jammy".to_string(), steps)
            }
        }
    }
}

fn ubuntu_official_plan(major: u32, minor: u32) -> (String, Vec<PackageStep>) {
    let mut steps = vec![
        PackageStep::root(
            "Enable the i386 architecture",
            "dpkg --add-architecture i386",
        ),
        PackageStep::root("Refresh package lists", "apt-get update"),
    ];
    if (major, minor) >= (26, 4) {
        steps.push(PackageStep::root(
            "Install Wine and dependencies",
            "apt-get install -y wine wine64 wine32 libwine libwine:i386 fonts-wine winetricks wget curl 7zip tar jq zstd xz-utils pciutils unzip",
        ));
        steps.push(dotnet_step("apt-get install -y dotnet-sdk-10.0"));
    } else {
        steps.push(PackageStep::root(
            "Install Wine and dependencies",
            "apt-get install -y wine wine64 wine32 libwine libwine:i386 fonts-wine winetricks wget curl p7zip-full tar jq zstd xz-utils pciutils unzip",
        ));
        steps.push(dotnet_step(
            "apt-get install -y dotnet-sdk-8.0 dotnet-sdk-10.0",
        ));
    }
    (format!("ubuntu-{major}.{minor}-official"), steps)
}

fn add_winehq_repo(steps: &mut Vec<PackageStep>, sources: &str) {
    steps.push(PackageStep::root(
        "Add the WineHQ signing key",
        format!(
            "mkdir -pm755 /etc/apt/keyrings && wget -O- {WINEHQ_KEY} | gpg --dearmor -o /etc/apt/keyrings/winehq-archive.key"
        ),
    ));
    steps.push(PackageStep::root(
        "Add the WineHQ repository",
        format!("wget -NP /etc/apt/sources.list.d/ {WINEHQ_BASE}/{sources}"),
    ));
    steps.push(PackageStep::root(
        "Enable i386 and refresh package lists",
        "dpkg --add-architecture i386 && apt-get update",
    ));
}

fn winehq_install_step(with_winetricks: bool) -> PackageStep {
    let winetricks = if with_winetricks { "winetricks " } else { "" };
    PackageStep::root(
        "Install WineHQ staging and dependencies",
        format!(
            "apt-get install --install-recommends -y winehq-staging {winetricks}wget curl p7zip-full tar jq zstd xz-utils pciutils unzip"
        ),
    )
}

fn dotnet_step(script: &str) -> PackageStep {
    PackageStep::root_optional("Install .NET SDKs (optional)", script)
}

fn fedora_plan() -> (String, Vec<PackageStep>) {
    (
        "fedora".to_string(),
        vec![
            PackageStep::root(
                "Install Wine and build dependencies",
                "dnf install -y wine winetricks wget curl p7zip p7zip-plugins tar jq zstd xz pciutils unzip",
            ),
            dotnet_step("dnf install -y dotnet-sdk-8.0 dotnet-sdk-10.0"),
        ],
    )
}

fn suse_plan() -> (String, Vec<PackageStep>) {
    (
        "opensuse".to_string(),
        vec![
            PackageStep::root("Refresh package lists", "zypper refresh"),
            PackageStep::root(
                "Install Wine and build dependencies",
                "zypper install -y wine winetricks wget curl 7zip tar jq zstd xz pciutils unzip",
            ),
            dotnet_step("zypper install -y dotnet-sdk-10.0"),
            dotnet_step("zypper install -y dotnet-sdk-8.0"),
        ],
    )
}

/// Optional OpenCL packages for AMD GPUs. Upstream only automates Fedora,
/// Arch-family and PikaOS; everything else gets a note.
pub fn amd_opencl_plan(os: &OsRelease) -> PackagePlan {
    let family = os.family();
    let id = os.id.to_ascii_lowercase();
    let (major, _) = version_tuple(os);
    let mut steps = Vec::new();
    let mut notes = Vec::new();

    match family {
        Family::Arch => steps.push(PackageStep::root_optional(
            "Install AMD OpenCL packages",
            "pacman -S --needed --noconfirm opencl-mesa ocl-icd rocm-opencl-runtime rocm-hip wine-opencl",
        )),
        Family::Fedora => {
            if id == "fedora" && major >= 43 {
                steps.push(PackageStep::root_optional(
                    "Install AMD OpenCL packages",
                    "dnf install -y mesa-opencl-icd ocl-icd rocm-opencl rocm-hip wine-opencl",
                ));
            } else {
                steps.push(PackageStep::root_optional(
                    "Install AMD OpenCL packages",
                    "dnf install -y rocm-opencl apr apr-util zlib libxcrypt-compat libcurl libcurl-devel mesa-libGLU",
                ));
            }
        }
        Family::Ubuntu | Family::Debian => {
            if id == "pika" || id == "pikaos" {
                steps.push(PackageStep::root_optional(
                    "Install AMD OpenCL packages",
                    "apt-get install -y mesa-opencl-icd ocl-icd-libopencl1 rocm-opencl-runtime rocm-hip-runtime",
                ));
            } else {
                notes.push(
                    "AMD OpenCL packages are not automated for this distribution; install mesa-opencl-icd and rocm-opencl manually."
                        .to_string(),
                );
            }
        }
        _ => notes.push(
            "AMD OpenCL packages are not automated for this distribution; install the Mesa/ROCm OpenCL ICDs manually."
                .to_string(),
        ),
    }

    PackagePlan {
        family,
        flavor: "amd-opencl".to_string(),
        steps,
        notes,
    }
}

fn version_tuple(os: &OsRelease) -> (u32, u32) {
    let Some(version) = os.version_id.as_deref() else {
        return (0, 0);
    };
    let mut parts = version.split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    None,
    Sudo,
    Pkexec,
}

pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|out| out.trim() == "0")
        .unwrap_or(false)
}

pub fn default_elevation() -> Elevation {
    if is_root() {
        Elevation::None
    } else {
        Elevation::Sudo
    }
}

#[derive(Debug, Clone)]
pub struct PlanRunOptions {
    pub dry_run: bool,
    pub elevation: Elevation,
    pub log_path: Option<PathBuf>,
}

impl Default for PlanRunOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            elevation: default_elevation(),
            log_path: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct PlanOutcome {
    pub completed: Vec<String>,
    pub failed_optional: Vec<String>,
}

pub fn run_plan<F>(plan: &PackagePlan, opts: &PlanRunOptions, mut on_line: F) -> Result<PlanOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut outcome = PlanOutcome::default();
    if plan.steps.is_empty() {
        on_line(
            Stream::Stdout,
            "no automatic package steps for this distribution",
        );
        return Ok(outcome);
    }

    let total = plan.steps.len();
    for (index, step) in plan.steps.iter().enumerate() {
        on_line(
            Stream::Stdout,
            &format!("==> [{}/{}] {}", index + 1, total, step.label),
        );

        if opts.dry_run {
            let flags = match (step.needs_root, step.optional) {
                (true, true) => "root, optional",
                (true, false) => "root",
                (false, true) => "user, optional",
                (false, false) => "user",
            };
            on_line(
                Stream::Stdout,
                &format!("    [{flags}] sh -c {}", shell_quote(&step.script)),
            );
            outcome.completed.push(step.label.clone());
            continue;
        }

        let cmd = build_command(step, opts.elevation)?;
        let mut run_opts = RunOptions::default()
            .stall(Duration::from_secs(1800))
            .log_append();
        if let Some(path) = &opts.log_path {
            run_opts = run_opts.log(path);
        }
        if step.needs_root && matches!(opts.elevation, Elevation::Sudo) {
            run_opts = run_opts.inherit_stdin();
        }

        let result = runner::run(cmd, run_opts, &mut on_line)?;
        if !result.success() {
            let detail = result.tail_text(15);
            if step.optional {
                on_line(
                    Stream::Stderr,
                    &format!("optional step failed, continuing: {}", step.label),
                );
                outcome.failed_optional.push(step.label.clone());
                continue;
            }
            bail!("step '{}' failed\n{detail}", step.label);
        }
        outcome.completed.push(step.label.clone());
    }

    Ok(outcome)
}

fn build_command(step: &PackageStep, elevation: Elevation) -> Result<Command> {
    if !step.needs_root || elevation == Elevation::None {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&step.script);
        return Ok(cmd);
    }

    let cmd = match elevation {
        Elevation::Sudo => {
            let mut cmd = Command::new("sudo");
            cmd.arg("sh").arg("-c").arg(&step.script);
            cmd
        }
        Elevation::Pkexec => {
            let mut cmd = Command::new("pkexec");
            cmd.arg("/bin/sh").arg("-c").arg(&step.script);
            cmd
        }
        Elevation::None => unreachable!("handled above"),
    };
    Ok(cmd)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(text: &str) -> PackagePlan {
        plan_with_multilib(&OsRelease::parse(text), Some(true))
    }

    fn scripts(plan: &PackagePlan) -> String {
        plan.steps
            .iter()
            .map(|step| step.script.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn arch_plan_uses_pacman() {
        let plan = plan_for("ID=cachyos\nID_LIKE=arch\n");
        assert_eq!(plan.family, Family::Arch);
        assert!(scripts(&plan).contains("pacman -Syu"));
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn arch_plan_adds_multilib_when_missing() {
        let plan = plan_with_multilib(&OsRelease::parse("ID=arch\n"), Some(false));
        assert!(plan.steps[0].script.contains("[multilib]"));
        assert!(plan.steps[0].script.contains("/etc/pacman.conf"));

        let plan = plan_with_multilib(&OsRelease::parse("ID=arch\n"), Some(true));
        assert!(!scripts(&plan).contains("[multilib]"));
    }

    #[test]
    fn ubuntu_2404_uses_official_packages() {
        let plan = plan_for("ID=ubuntu\nVERSION_ID=\"24.04\"\nID_LIKE=debian\n");
        assert_eq!(plan.flavor, "ubuntu-24.4-official");
        let scripts = scripts(&plan);
        assert!(scripts.contains("dpkg --add-architecture i386"));
        assert!(scripts.contains("p7zip-full"));
        assert!(!scripts.contains("winehq-staging"));
    }

    #[test]
    fn ubuntu_2604_uses_7zip_and_no_dotnet8() {
        let plan = plan_for("ID=ubuntu\nVERSION_ID=\"26.04\"\n");
        let scripts = scripts(&plan);
        assert!(scripts.contains(" 7zip "));
        assert!(!scripts.contains("p7zip-full"));
        assert!(scripts.contains("dotnet-sdk-10.0"));
        assert!(!scripts.contains("dotnet-sdk-8.0"));
    }

    #[test]
    fn old_ubuntu_uses_winehq() {
        let plan = plan_for("ID=ubuntu\nVERSION_ID=\"22.04\"\nID_LIKE=debian\n");
        assert_eq!(plan.flavor, "ubuntu-winehq-jammy");
        assert!(scripts(&plan).contains("winehq-jammy.sources"));
        assert!(scripts(&plan).contains("winehq-staging"));
    }

    #[test]
    fn mint_zorin_and_pop_use_winehq_repos() {
        let mint = plan_for("ID=linuxmint\nVERSION_ID=\"22\"\nID_LIKE=\"ubuntu debian\"\n");
        assert_eq!(mint.flavor, "mint-22-winehq-noble");
        assert!(scripts(&mint).contains("winehq-noble.sources"));

        let zorin = plan_for("ID=zorin\nVERSION_ID=\"17\"\nID_LIKE=ubuntu\n");
        assert_eq!(zorin.flavor, "zorin-17-winehq-jammy");

        let pop = plan_for("ID=pop\nVERSION_ID=\"22.04\"\nID_LIKE=\"ubuntu debian\"\n");
        assert_eq!(pop.flavor, "pop-winehq-jammy");
        assert!(!scripts(&pop).contains("dotnet-sdk-10.0"));
    }

    #[test]
    fn pikaos_builds_winetricks_from_source() {
        let plan = plan_for("ID=pika\nID_LIKE=\"ubuntu debian\"\n");
        assert_eq!(plan.flavor, "pikaos-winehq-forky");
        let scripts = scripts(&plan);
        assert!(scripts.contains("winehq-forky.sources"));
        assert!(scripts.contains("Winetricks/winetricks"));
        assert!(!scripts.contains("winehq-staging winetricks"));
    }

    #[test]
    fn fedora_and_suse_plans() {
        let fedora = plan_for("ID=fedora\n");
        assert!(scripts(&fedora).contains("dnf install"));
        assert!(scripts(&fedora).contains("p7zip-plugins"));

        let suse = plan_for("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n");
        assert!(scripts(&suse).contains("zypper install"));
        assert!(scripts(&suse).contains("7zip"));
    }

    #[test]
    fn unknown_and_bazzite_have_no_steps() {
        let unknown = plan_for("ID=void\n");
        assert!(unknown.steps.is_empty());
        assert!(!unknown.notes.is_empty());

        let bazzite = plan_for("ID=bazzite\nID_LIKE=fedora\n");
        assert!(bazzite.steps.is_empty());
        assert!(bazzite.notes.iter().any(|note| note.contains("immutable")));
    }

    #[test]
    fn dry_run_does_not_execute() {
        let plan = plan_for("ID=arch\n");
        let opts = PlanRunOptions {
            dry_run: true,
            elevation: Elevation::None,
            log_path: None,
        };
        let mut lines = Vec::new();
        let outcome = run_plan(&plan, &opts, |_, line| lines.push(line.to_string())).unwrap();
        assert_eq!(outcome.completed.len(), plan.steps.len());
        assert!(lines.iter().any(|line| line.contains("pacman -Syu")));
    }
}
