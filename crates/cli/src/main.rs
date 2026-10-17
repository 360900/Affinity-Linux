use anyhow::Result;
use clap::{Parser, Subcommand};

use affinity_linux_core::config::Config;
use affinity_linux_core::doctor::{self, DoctorReport};

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
    /// Install Affinity v3 into the configured prefix
    Install {
        /// Print the planned commands without changing the system
        #[arg(long)]
        dry_run: bool,
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
        Command::Install { dry_run: _ } => {
            anyhow::bail!(
                "install is not implemented yet (M2-M4); run `affinity-linux doctor` for the system check"
            )
        }
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

    for error in &report.errors {
        println!("  error         {error}");
    }
}
