slint::include_modules!();

use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use affinity_linux_core::app_exe;
use affinity_linux_core::backends;
use affinity_linux_core::config::{Config, GpuBackend, default_prefix};
use affinity_linux_core::deps;
use affinity_linux_core::distro::OsRelease;
use affinity_linux_core::doctor::{self, DoctorReport};
use affinity_linux_core::gpu::{self, Vendor};
use affinity_linux_core::install;
use affinity_linux_core::launch;
use affinity_linux_core::manifest::now_epoch;
use affinity_linux_core::packages::{self, Elevation, PlanRunOptions};
use affinity_linux_core::runner::Stream;
use affinity_linux_core::uninstall;
use affinity_linux_core::wine;
use slint::{ComponentHandle, Model, VecModel};

enum UiEvent {
    Doctor(Box<DoctorReport>),
    Log(String, bool),
    JobDone(&'static str),
    JobFailed(String),
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    let cfg = Config::load().unwrap_or_else(|err| {
        eprintln!("config: {err:#}");
        Config::default()
    });

    let logs: Rc<VecModel<LogLine>> = Rc::new(VecModel::from(Vec::new()));
    ui.set_logs(logs.clone().into());
    ui.set_prefix_text(cfg.prefix.display().to_string().into());
    ui.set_backend_index(if cfg.gpu_backend == GpuBackend::Dxvk {
        1
    } else {
        0
    });
    ui.set_opencl_enabled(cfg.opencl);

    let (tx, rx) = mpsc::channel::<UiEvent>();
    spawn_doctor(cfg.clone(), tx.clone());

    {
        let weak = ui.as_weak();
        let cfg = cfg.clone();
        let tx = tx.clone();
        ui.on_install_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let cfg = Config::load().unwrap_or_else(|_| cfg.clone());
            let backend = if ui.get_backend_index() == 1 {
                GpuBackend::Dxvk
            } else {
                GpuBackend::Vkd3d
            };
            let opencl = ui.get_opencl_enabled();
            ui.set_busy(true);
            ui.set_state_text("Installing Affinity...".into());
            ui.set_status_text("Starting...".into());
            spawn_install(cfg, backend, opencl, tx.clone());
        });
    }
    {
        let weak = ui.as_weak();
        let cfg = cfg.clone();
        let tx = tx.clone();
        ui.on_launch_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let cfg = Config::load().unwrap_or_else(|_| cfg.clone());
            ui.set_busy(true);
            ui.set_state_text("Opening Affinity...".into());
            ui.set_status_text("Starting...".into());
            spawn_launch(cfg, tx.clone());
        });
    }
    {
        let weak = ui.as_weak();
        let cfg = cfg.clone();
        let tx = tx.clone();
        ui.on_update_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let cfg = Config::load().unwrap_or_else(|_| cfg.clone());
            ui.set_busy(true);
            ui.set_state_text("Updating Affinity...".into());
            ui.set_status_text("Starting...".into());
            spawn_update(cfg, tx.clone());
        });
    }
    {
        let weak = ui.as_weak();
        let cfg = cfg.clone();
        let tx = tx.clone();
        ui.on_repair_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let cfg = Config::load().unwrap_or_else(|_| cfg.clone());
            let backend = if ui.get_backend_index() == 1 {
                GpuBackend::Dxvk
            } else {
                GpuBackend::Vkd3d
            };
            let opencl = ui.get_opencl_enabled();
            ui.set_busy(true);
            ui.set_state_text("Repairing Affinity...".into());
            ui.set_status_text("Starting...".into());
            spawn_repair(cfg, backend, opencl, tx.clone());
        });
    }
    {
        let weak = ui.as_weak();
        let cfg = cfg.clone();
        let tx = tx.clone();
        ui.on_uninstall_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let cfg = Config::load().unwrap_or_else(|_| cfg.clone());
            ui.set_busy(true);
            ui.set_state_text("Removing Affinity...".into());
            ui.set_status_text("Starting...".into());
            spawn_uninstall(cfg, tx.clone());
        });
    }

    let weak = ui.as_weak();
    let logs = logs.clone();
    let tx = tx.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            while let Ok(event) = rx.try_recv() {
                match event {
                    UiEvent::Doctor(report) => apply_report(&ui, &logs, *report),
                    UiEvent::Log(text, error) => {
                        ui.set_status_text(text.clone().into());
                        push_log(&logs, text, error);
                    }
                    UiEvent::JobDone(kind) => {
                        ui.set_busy(false);
                        ui.set_state_text(job_state(kind).into());
                        ui.set_status_text("Done".into());
                        push_log(&logs, format!("{kind}: finished"), false);
                        let cfg = Config::load().unwrap_or_default();
                        spawn_doctor(cfg, tx.clone());
                    }
                    UiEvent::JobFailed(message) => {
                        ui.set_busy(false);
                        ui.set_state_text("Something went wrong.".into());
                        ui.set_status_text(message.clone().into());
                        ui.set_details_open(true);
                        push_log(&logs, format!("error: {message}"), true);
                    }
                }
            }
        },
    );

    ui.run()
}

fn job_state(kind: &str) -> &'static str {
    match kind {
        "install" => "Affinity is installed.",
        "update" => "Affinity is up to date.",
        "repair" => "Affinity has been repaired.",
        "uninstall" => "Affinity has been removed.",
        "launch" => "Affinity is running.",
        _ => "Done.",
    }
}

fn push_log(logs: &Rc<VecModel<LogLine>>, text: String, error: bool) {
    while logs.row_count() >= 400 {
        logs.remove(0);
    }
    logs.push(LogLine {
        text: text.into(),
        error,
    });
}

fn spawn_doctor(cfg: Config, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let report = doctor::run(&cfg);
        let _ = tx.send(UiEvent::Doctor(Box::new(report)));
    });
}

fn spawn_install(cfg: Config, backend: GpuBackend, opencl: bool, tx: Sender<UiEvent>) {
    std::thread::spawn(move || match install_all(&cfg, backend, opencl, &tx) {
        Ok(()) => {
            let _ = tx.send(UiEvent::JobDone("install"));
        }
        Err(error) => {
            let _ = tx.send(UiEvent::JobFailed(format!("{error:#}")));
        }
    });
}

fn install_all(
    cfg: &Config,
    backend: GpuBackend,
    opencl: bool,
    tx: &Sender<UiEvent>,
) -> anyhow::Result<()> {
    let os = OsRelease::detect()?;
    let prefix = cfg.prefix.clone();
    fs::create_dir_all(&prefix)?;
    let log_dir = prefix.join("logs");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("install-{}.log", now_epoch()));
    let mut send = |stream: Stream, line: &str| {
        let _ = tx.send(UiEvent::Log(
            line.to_string(),
            matches!(stream, Stream::Stderr),
        ));
    };

    let checks = deps::check_all();
    let missing = deps::missing(&checks);
    if !missing.is_empty() {
        let plan = packages::plan(&os);
        if plan.steps.is_empty() {
            let _ = tx.send(UiEvent::Log(
                format!(
                    "warning: no automatic dependency plan; missing: {}",
                    missing
                        .iter()
                        .map(|dep| dep.command)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                true,
            ));
        } else {
            let elevation = if packages::is_root() {
                Elevation::None
            } else {
                Elevation::Pkexec
            };
            let options = PlanRunOptions {
                dry_run: false,
                elevation,
                log_path: Some(log_path.clone()),
            };
            packages::run_plan(&plan, &options, &mut send)?;
        }
    }

    if opencl && gpu::detect().iter().any(|gpu| gpu.vendor == Vendor::Amd) {
        let plan = packages::amd_opencl_plan(&os);
        if !plan.steps.is_empty() {
            let elevation = if packages::is_root() {
                Elevation::None
            } else {
                Elevation::Pkexec
            };
            let options = PlanRunOptions {
                dry_run: false,
                elevation,
                log_path: Some(log_path.clone()),
            };
            let _ = packages::run_plan(&plan, &options, &mut send);
        }
    }

    let setup = wine::SetupOptions {
        prefix: prefix.clone(),
        family: os.family(),
        dry_run: false,
        log_path: Some(log_path.clone()),
        wine_version: None,
    };
    wine::setup(&setup, &mut send)?;

    ensure_installed(&prefix)?;

    let backend_options = backends::BackendOptions {
        prefix: prefix.clone(),
        backend,
        opencl,
        dry_run: false,
        log_path: Some(log_path.clone()),
    };
    if let Err(error) = backends::install(&backend_options, &mut send) {
        let _ = tx.send(UiEvent::Log(
            format!("warning: GPU backend setup failed: {error:#}"),
            true,
        ));
    }

    let install_options = install::InstallOptions {
        prefix: prefix.clone(),
        installer: None,
        dry_run: false,
        log_path: Some(log_path),
    };
    install::install(&install_options, &mut send)?;

    let mut saved = cfg.clone();
    saved.gpu_backend = backend;
    saved.opencl = opencl;
    saved.save()?;
    Ok(())
}

fn ensure_installed(prefix: &Path) -> anyhow::Result<()> {
    if !app_exe(prefix).exists() {
        anyhow::bail!(
            "Affinity is not installed in {}; run the installer again",
            prefix.display()
        );
    }
    let registry = fs::read_to_string(prefix.join("system.reg")).unwrap_or_default();
    if !wine::has_dotnet48(&registry) {
        anyhow::bail!(".NET Framework 4.8 is missing from the prefix; reinstall Affinity");
    }
    Ok(())
}

fn spawn_update(cfg: Config, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let prefix = cfg.prefix.clone();
        let result = (|| -> anyhow::Result<()> {
            ensure_installed(&prefix)?;
            let log_path = prefix
                .join("logs")
                .join(format!("update-{}.log", now_epoch()));
            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut send = |stream: Stream, line: &str| {
                let _ = tx.send(UiEvent::Log(
                    line.to_string(),
                    matches!(stream, Stream::Stderr),
                ));
            };
            let options = install::InstallOptions {
                prefix,
                installer: None,
                dry_run: false,
                log_path: Some(log_path),
            };
            install::install(&options, &mut send)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                let _ = tx.send(UiEvent::JobDone("update"));
            }
            Err(error) => {
                let _ = tx.send(UiEvent::JobFailed(format!("{error:#}")));
            }
        }
    });
}

fn spawn_repair(cfg: Config, backend: GpuBackend, opencl: bool, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let prefix = cfg.prefix.clone();
        let result = (|| -> anyhow::Result<()> {
            ensure_installed(&prefix)?;
            let log_path = prefix
                .join("logs")
                .join(format!("repair-{}.log", now_epoch()));
            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut send = |stream: Stream, line: &str| {
                let _ = tx.send(UiEvent::Log(
                    line.to_string(),
                    matches!(stream, Stream::Stderr),
                ));
            };
            let backend_options = backends::BackendOptions {
                prefix: prefix.clone(),
                backend,
                opencl,
                dry_run: false,
                log_path: Some(log_path.clone()),
            };
            if let Err(error) = backends::install(&backend_options, &mut send) {
                let _ = tx.send(UiEvent::Log(
                    format!("warning: GPU backend setup failed: {error:#}"),
                    true,
                ));
            }
            let options = install::InstallOptions {
                prefix,
                installer: None,
                dry_run: false,
                log_path: Some(log_path),
            };
            install::repair(&options, &mut send)?;
            let mut saved = cfg.clone();
            saved.gpu_backend = backend;
            saved.opencl = opencl;
            saved.save()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                let _ = tx.send(UiEvent::JobDone("repair"));
            }
            Err(error) => {
                let _ = tx.send(UiEvent::JobFailed(format!("{error:#}")));
            }
        }
    });
}

fn spawn_launch(cfg: Config, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let prefix = cfg.prefix.clone();
        let log_path = prefix
            .join("logs")
            .join(format!("launch-{}.log", now_epoch()));
        let options = launch::LaunchOptions {
            prefix,
            args: Vec::new(),
            dry_run: false,
            log_path: Some(log_path),
        };
        let mut send = |stream: Stream, line: &str| {
            let _ = tx.send(UiEvent::Log(
                line.to_string(),
                matches!(stream, Stream::Stderr),
            ));
        };
        match launch::launch(&options, &mut send) {
            Ok(outcome) => {
                let _ = tx.send(UiEvent::Log(
                    format!("launched {}", outcome.target.display()),
                    false,
                ));
                let _ = tx.send(UiEvent::JobDone("launch"));
            }
            Err(error) => {
                let _ = tx.send(UiEvent::JobFailed(format!("{error:#}")));
            }
        }
    });
}

fn spawn_uninstall(cfg: Config, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let prefix = cfg.prefix.clone();
        let log_path = prefix
            .join("logs")
            .join(format!("uninstall-{}.log", now_epoch()));
        let options = uninstall::UninstallOptions {
            prefix,
            dry_run: false,
            log_path: Some(log_path),
        };
        let mut send = |stream: Stream, line: &str| {
            let _ = tx.send(UiEvent::Log(
                line.to_string(),
                matches!(stream, Stream::Stderr),
            ));
        };
        match uninstall::uninstall(&options, &mut send) {
            Ok(outcome) => {
                let mut saved = cfg.clone();
                saved.prefix = default_prefix();
                if let Err(error) = saved.save() {
                    let _ = tx.send(UiEvent::Log(format!("warning: {error:#}"), true));
                }
                let _ = tx.send(UiEvent::Log(
                    format!(
                        "removed {} desktop entries, prefix removed={}",
                        outcome.removed_entries, outcome.prefix_removed
                    ),
                    false,
                ));
                let _ = tx.send(UiEvent::JobDone("uninstall"));
            }
            Err(error) => {
                let _ = tx.send(UiEvent::JobFailed(format!("{error:#}")));
            }
        }
    });
}

fn apply_report(ui: &MainWindow, logs: &Rc<VecModel<LogLine>>, report: DoctorReport) {
    let package_manager = report.package_manager.as_deref().unwrap_or("not found");
    let distro = format!(
        "{} ({} family, {})",
        report.os.name(),
        report.family.label(),
        package_manager
    );
    let gpu = if report.gpus.is_empty() {
        "none detected".to_string()
    } else {
        report
            .gpus
            .iter()
            .map(|gpu| format!("{} [{}]", gpu.name, gpu.vendor.label()))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let found = report.deps.iter().filter(|dep| dep.found).count();
    let missing: Vec<&str> = report
        .deps
        .iter()
        .filter(|dep| dep.required && !dep.found)
        .map(|dep| dep.command)
        .collect();
    let deps = if missing.is_empty() {
        "all required dependencies present".to_string()
    } else {
        format!("missing: {}", missing.join(", "))
    };
    let wine = report
        .wine_build
        .clone()
        .unwrap_or_else(|| "not installed".into());

    ui.set_distro_text(distro.clone().into());
    ui.set_gpu_text(gpu.clone().into());
    ui.set_deps_text(deps.clone().into());
    ui.set_wine_text(wine.clone().into());
    ui.set_prefix_text(report.prefix.display().to_string().into());
    ui.set_installed(report.installed);

    let state = if report.installed {
        "Affinity is ready to use."
    } else {
        "Install Affinity to get started."
    };
    ui.set_state_text(state.into());
    if !ui.get_busy() {
        ui.set_status_text("System ready".into());
    }

    let missing_required = !missing.is_empty();
    push_log(logs, format!("system: {distro}"), false);
    push_log(logs, format!("gpu: {gpu}"), false);
    push_log(logs, format!("wine: {wine}"), false);
    push_log(
        logs,
        format!("deps: {found}/{} found, {deps}", report.deps.len()),
        missing_required,
    );
    for error in &report.errors {
        push_log(logs, format!("error: {error}"), true);
    }
}
