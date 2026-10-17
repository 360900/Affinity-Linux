use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use affinity_linux_core::config::Config;
use affinity_linux_core::deps;
use affinity_linux_core::distro::OsRelease;
use affinity_linux_core::doctor::{self, DoctorReport};
use affinity_linux_core::install;
use affinity_linux_core::manifest::{
    Manifest, STEP_DEPS, STEP_PREFIX, STEP_WINE, STEP_WINETRICKS, StepStatus, now_epoch,
};
use affinity_linux_core::packages::{self, Elevation, PlanRunOptions};
use affinity_linux_core::runner::Stream;
use affinity_linux_core::wine;

#[derive(Parser)]
#[command(
    name = "affinity-linux",
    version,
    about = "One-click Affinity v3 installer for Linux"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show distro, GPU, dependency and prefix status
    Doctor {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Check and install system packages needed by the installer
    Deps {
        /// Print the planned commands without changing the system
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
        /// Elevate with pkexec instead of sudo
        #[arg(long)]
        pkexec: bool,
    },
    /// Download and configure the patched Wine build
    WineSetup {
        /// Print the planned commands without downloading anything
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
        /// Wine build to install (11.19 or 11.18)
        #[arg(long)]
        version: Option<String>,
        /// Prefix override; does not change the saved config
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Install Affinity v3 into the configured prefix
    Install {
        /// Print the planned commands without changing the system
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
        /// Prefix override; saved to the config after a successful install
        #[arg(long)]
        prefix: Option<PathBuf>,
        /// Use a local Affinity setup executable instead of downloading it
        #[arg(long)]
        installer: Option<PathBuf>,
        /// Skip the system dependency installation step
        #[arg(long)]
        skip_deps: bool,
        /// Skip the Wine bootstrap step (use the existing prefix)
        #[arg(long)]
        skip_wine: bool,
    },
    /// Launch Affinity with the correct Wine environment
    Launch {
        /// Extra arguments forwarded to the Affinity executable
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Remove the prefix, desktop entries and stored config
    Uninstall {
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;

    match cli.command {
        Command::Doctor { json } => {
            let report = doctor::run(&cfg);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_report(&report);
            }
            Ok(())
        }
        Command::Deps {
            dry_run,
            yes,
            pkexec,
        } => run_deps(&cfg, dry_run, yes, pkexec),
        Command::WineSetup {
            dry_run,
            yes,
            version,
            prefix,
        } => run_wine_setup(&cfg, dry_run, yes, version, prefix),
        Command::Install {
            dry_run,
            yes,
            prefix,
            installer,
            skip_deps,
            skip_wine,
        } => run_install(&cfg, dry_run, yes, prefix, installer, skip_deps, skip_wine),
        Command::Launch { args: _ } => {
            anyhow::bail!("launch is not implemented yet (M5); the prefix is untouched")
        }
        Command::Uninstall { yes: _ } => {
            anyhow::bail!("uninstall is not implemented yet (M5); nothing was removed")
        }
    }
}

fn print_report(report: &DoctorReport) {
    println!("affinity-linux doctor");
    println!(
        "  distro        {} (id={}, version={}, family={}, pkg={})",
        report.os.name(),
        report.os.id,
        report.os.version_id.as_deref().unwrap_or("-"),
        report.family.label(),
        report.package_manager.as_deref().unwrap_or("not found"),
    );

    if report.gpus.is_empty() {
        println!("  gpus          none detected");
    } else {
        for gpu in &report.gpus {
            println!("  gpu           {} [{}]", gpu.name, gpu.vendor.label());
        }
    }

    if report.vulkan_icds.is_empty() {
        println!("  vulkan        no ICD manifest found (install a Vulkan driver)");
    } else {
        println!("  vulkan        {}", report.vulkan_icds.join(", "));
    }

    println!(
        "  wine build    {}",
        report.wine_build.as_deref().unwrap_or("not installed")
    );
    println!(
        "  prefix        {} (exists={}, installed={})",
        report.prefix.display(),
        report.prefix_exists,
        report.installed
    );
    println!("  config        {}", report.config_path.display());

    let found = report.deps.iter().filter(|dep| dep.found).count();
    println!("  dependencies  {}/{} found", found, report.deps.len());
    for dep in &report.deps {
        let state = if dep.found {
            "ok"
        } else if dep.required {
            "MISSING"
        } else {
            "optional"
        };
        let version = dep.version.as_deref().unwrap_or("");
        println!("    {:<11} {:<9} {}", dep.command, state, version);
    }

    if let Some(manifest) = &report.manifest {
        println!(
            "  manifest      {} steps recorded, incomplete: {}",
            manifest.steps.len(),
            manifest.incomplete_steps().len()
        );
    }

    for note in &report.package_notes {
        println!("  note          {note}");
    }

    for error in &report.errors {
        println!("  error         {error}");
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn run_wine_setup(
    cfg: &Config,
    dry_run: bool,
    yes: bool,
    version: Option<String>,
    prefix: Option<PathBuf>,
) -> Result<()> {
    if let Some(version) = &version
        && version != wine::WINE_VERSION
        && version != wine::WINE_FALLBACK_VERSION
    {
        bail!(
            "unsupported Wine version '{version}' (expected {} or {})",
            wine::WINE_VERSION,
            wine::WINE_FALLBACK_VERSION
        );
    }

    let os = OsRelease::detect()?;
    let prefix = prefix.unwrap_or_else(|| cfg.prefix.clone());

    println!(
        "wine setup for {} ({}, prefix {})",
        os.name(),
        os.family().label(),
        prefix.display()
    );
    println!(
        "  profile {}",
        version
            .as_deref()
            .map(|version| format!("Wine {version}"))
            .unwrap_or_else(|| format!(
                "Wine {} (fallback {})",
                wine::WINE_VERSION,
                wine::WINE_FALLBACK_VERSION
            ))
    );

    if !dry_run && !yes && !confirm("Download and configure the patched Wine build?")? {
        println!("aborted");
        return Ok(());
    }

    let log_path = if dry_run {
        None
    } else {
        let log_dir = prefix.join("logs");
        fs::create_dir_all(&log_dir)?;
        let path = log_dir.join(format!("wine-{}.log", now_epoch()));
        println!("  log     {}", path.display());
        Some(path)
    };

    let options = wine::SetupOptions {
        prefix: prefix.clone(),
        family: os.family(),
        dry_run,
        log_path,
        wine_version: version,
    };

    let report = wine::setup(&options, |stream, line| match stream {
        Stream::Stdout => println!("{line}"),
        Stream::Stderr => eprintln!("{line}"),
    })?;

    if let Some(version) = &report.wine_version {
        println!("wine    {version}");
    }
    if let Some(backup) = &report.backup {
        println!("backup  {}", backup.display());
    }
    if !report.failed_components.is_empty() {
        println!(
            "warning: components failed: {}",
            report.failed_components.join(", ")
        );
    }

    Ok(())
}

fn print_stream(stream: Stream, line: &str) {
    match stream {
        Stream::Stdout => println!("{line}"),
        Stream::Stderr => eprintln!("{line}"),
    }
}

fn mark_step(prefix: &Path, step: &str, status: StepStatus) {
    if let Ok(mut manifest) = Manifest::load_or_new(prefix) {
        manifest.mark(step, status, None);
        let _ = manifest.save(prefix);
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "ok" } else { "skipped" }
}

fn run_install(
    cfg: &Config,
    dry_run: bool,
    yes: bool,
    prefix: Option<PathBuf>,
    installer: Option<PathBuf>,
    skip_deps: bool,
    skip_wine: bool,
) -> Result<()> {
    let os = OsRelease::detect()?;
    let prefix = prefix.unwrap_or_else(|| cfg.prefix.clone());

    println!("install Affinity v3");
    println!("  distro    {} ({})", os.name(), os.family().label());
    println!("  prefix    {}", prefix.display());
    println!(
        "  installer {}",
        installer
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "download from affinity.studio".to_string())
    );
    println!(
        "  deps      {}",
        if skip_deps {
            "skipped"
        } else {
            "install if missing"
        }
    );
    println!(
        "  wine      {}",
        if skip_wine {
            "reuse the existing prefix"
        } else {
            "bootstrap 11.19 (fallback 11.18)"
        }
    );
    println!();

    if dry_run {
        if !skip_deps {
            let plan = packages::plan(&os);
            let options = PlanRunOptions {
                dry_run: true,
                elevation: packages::default_elevation(),
                log_path: None,
            };
            packages::run_plan(&plan, &options, print_stream)?;
        }
        if !skip_wine {
            let options = wine::SetupOptions {
                prefix: prefix.clone(),
                family: os.family(),
                dry_run: true,
                log_path: None,
                wine_version: None,
            };
            wine::setup(&options, print_stream)?;
        }
        let options = install::InstallOptions {
            prefix,
            installer,
            dry_run: true,
            log_path: None,
        };
        install::install(&options, print_stream)?;
        return Ok(());
    }

    if !yes && !confirm(&format!("Install Affinity v3 into {}?", prefix.display()))? {
        println!("aborted");
        return Ok(());
    }

    fs::create_dir_all(&prefix)?;
    let log_dir = prefix.join("logs");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("install-{}.log", now_epoch()));
    println!("  log       {}", log_path.display());

    if !skip_deps {
        let checks = deps::check_all();
        let missing = deps::missing(&checks);
        if missing.is_empty() {
            println!("dependencies are already present");
        } else {
            println!(
                "installing missing dependencies: {}",
                missing
                    .iter()
                    .map(|dep| dep.command)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let plan = packages::plan(&os);
            if plan.steps.is_empty() {
                println!("warning: no automatic dependency plan for this distribution");
            } else {
                let elevation = packages::default_elevation();
                if elevation == Elevation::Sudo && deps::which("sudo").is_none() {
                    bail!("sudo is missing; install the dependencies manually or run as root");
                }
                let options = PlanRunOptions {
                    dry_run: false,
                    elevation,
                    log_path: Some(log_path.clone()),
                };
                packages::run_plan(&plan, &options, print_stream)?;
            }
        }
    }

    if !skip_wine {
        let options = wine::SetupOptions {
            prefix: prefix.clone(),
            family: os.family(),
            dry_run: false,
            log_path: Some(log_path.clone()),
            wine_version: None,
        };
        let setup = wine::setup(&options, print_stream)?;
        if let Some(version) = &setup.wine_version {
            println!("wine      {version}");
        }
        if !setup.failed_components.is_empty() {
            println!(
                "warning: components failed: {}",
                setup.failed_components.join(", ")
            );
        }
        mark_step(&prefix, STEP_DEPS, StepStatus::Done);
        mark_step(&prefix, STEP_PREFIX, StepStatus::Done);
        mark_step(&prefix, STEP_WINE, StepStatus::Done);
        mark_step(&prefix, STEP_WINETRICKS, StepStatus::Done);
    }

    let registry = fs::read_to_string(prefix.join("system.reg")).unwrap_or_default();
    if !wine::has_dotnet48(&registry) {
        bail!(
            ".NET Framework 4.8 is not registered in {}; run `affinity-linux wine-setup` first",
            prefix.display()
        );
    }

    let options = install::InstallOptions {
        prefix: prefix.clone(),
        installer,
        dry_run: false,
        log_path: Some(log_path.clone()),
    };
    let outcome = install::install(&options, print_stream)?;

    if outcome.used_msi_fallback {
        println!("note: the embedded MSI fallback was used");
    }
    println!("  facades       {}", yes_no(outcome.facades));
    println!("  settings patch {}", yes_no(outcome.patched));
    println!("  plugin loader {}", yes_no(outcome.pluginloader));
    println!("  settings seed {}", yes_no(outcome.settings));
    println!("  webview2      {}", yes_no(outcome.webview2));
    println!("  url handler   {}", yes_no(outcome.url_handler));

    if !outcome.failures.is_empty() {
        println!("completed with {} warning(s):", outcome.failures.len());
        for failure in &outcome.failures {
            println!("  - {failure}");
        }
    }

    let mut saved = cfg.clone();
    saved.prefix = prefix;
    saved.save()?;
    println!("saved install location in the config");
    Ok(())
}

fn run_deps(cfg: &Config, dry_run: bool, yes: bool, pkexec: bool) -> Result<()> {
    let os = OsRelease::detect()?;
    let plan = packages::plan(&os);

    println!(
        "dependency plan for {} ({})",
        os.name(),
        plan.family.label()
    );
    println!("  flavor  {}", plan.flavor);
    println!("  steps   {}", plan.steps.len());
    for note in &plan.notes {
        println!("  note    {note}");
    }

    if plan.steps.is_empty() {
        let checks = deps::check_all();
        let missing = deps::missing(&checks);
        if missing.is_empty() {
            println!("all required dependencies are present");
            return Ok(());
        }
        bail!(
            "no automatic plan for this distribution; still missing: {}",
            missing
                .iter()
                .map(|dep| dep.command)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let print_line = |stream: Stream, line: &str| match stream {
        Stream::Stdout => println!("{line}"),
        Stream::Stderr => eprintln!("{line}"),
    };

    if dry_run {
        let options = PlanRunOptions {
            dry_run: true,
            elevation: packages::default_elevation(),
            log_path: None,
        };
        packages::run_plan(&plan, &options, print_line)?;
        return Ok(());
    }

    if !yes && !confirm("Proceed?")? {
        println!("aborted");
        return Ok(());
    }

    let elevation = if pkexec {
        Elevation::Pkexec
    } else if packages::is_root() {
        Elevation::None
    } else {
        Elevation::Sudo
    };
    match elevation {
        Elevation::Pkexec if deps::which("pkexec").is_none() => {
            bail!("pkexec was not found on PATH")
        }
        Elevation::Sudo if deps::which("sudo").is_none() => {
            bail!("sudo was not found; run as root or install sudo")
        }
        _ => {}
    }

    let log_dir = cfg.prefix.join("logs");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("deps-{}.log", now_epoch()));
    println!("log     {}", log_path.display());

    let options = PlanRunOptions {
        dry_run: false,
        elevation,
        log_path: Some(log_path),
    };
    let outcome = packages::run_plan(&plan, &options, print_line)?;
    println!("completed {} steps", outcome.completed.len());
    if !outcome.failed_optional.is_empty() {
        println!(
            "optional steps failed: {}",
            outcome.failed_optional.join(", ")
        );
    }

    let checks = deps::check_all();
    let missing = deps::missing(&checks);
    if missing.is_empty() {
        println!("all required dependencies are present");
        Ok(())
    } else {
        bail!(
            "still missing: {}",
            missing
                .iter()
                .map(|dep| dep.command)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}
