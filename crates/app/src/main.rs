slint::include_modules!();

use std::cell::Cell;
use std::fs;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

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
use slint::{ComponentHandle, VecModel};

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
        let logs = logs.clone();
        let cfg = cfg.clone();
        let tx = tx.clone();
        ui.on_install_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let backend = if ui.get_backend_index() == 1 {
                GpuBackend::Dxvk
            } else {
                GpuBackend::Vkd3d
            };
            let opencl = ui.get_opencl_enabled();
            ui.set_busy(true);
            ui.set_state_text("Installing Affinity v3...".into());
            push_log(
                &logs,
                format!(
                    "install: starting (backend {}, opencl {})",
                    if backend == GpuBackend::Dxvk {
                        "dxvk"
                    } else {
                        "vkd3d"
                    },
                    opencl
                ),
                false,
            );
            spawn_install(cfg.clone(), backend, opencl, tx.clone());
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
            ui.set_busy(true);
            ui.set_state_text("Launching Affinity...".into());
            spawn_launch(cfg.clone(), tx.clone());
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        let cfg = cfg.clone();
        let tx = tx.clone();
        let pending = Rc::new(Cell::new(false));
        ui.on_uninstall_clicked(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !pending.get() {
                pending.set(true);
                ui.set_state_text("Click Uninstall again to confirm".into());
                push_log(
                    &logs,
                    "uninstall: press Uninstall again to confirm".into(),
                    true,
                );
                return;
            }
            pending.set(false);
            ui.set_busy(true);
            ui.set_state_text("Removing Affinity...".into());
            spawn_uninstall(cfg.clone(), tx.clone());
        });
    }
    {
        let logs_update = logs.clone();
        ui.on_update_clicked(move || {
            logs_update.push(LogLine {
                text: "update: deferred to v0.2".into(),
                error: false,
            });
        });
    }
    {
        let logs_repair = logs.clone();
        ui.on_repair_clicked(move || {
            logs_repair.push(LogLine {
                text: "repair: deferred to v0.2".into(),
                error: false,
            });
        });
    }
    {
        let weak = ui.as_weak();
        let tx = tx.clone();
        let cfg = cfg.clone();
        ui.on_recheck_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_state_text("Checking system...".into());
                ui.set_busy(true);
            }
            spawn_doctor(cfg.clone(), tx.clone());
        });
    }

    let weak = ui.as_weak();
    let logs = logs.clone();
    let cfg = cfg.clone();
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
                    UiEvent::Log(text, error) => push_log(&logs, text, error),
                    UiEvent::JobDone(kind) => {
                        ui.set_busy(false);
                        ui.set_state_text(format!("{kind} finished").into());
                        push_log(&logs, format!("{kind}: finished"), false);
                        spawn_doctor(cfg.clone(), tx.clone());
                    }
                    UiEvent::JobFailed(message) => {
                        ui.set_busy(false);
                        ui.set_state_text("Last action failed".into());
                        push_log(&logs, format!("error: {message}"), true);
                    }
                }
            }
        },
    );

    ui.run()
}

fn push_log(logs: &Rc<VecModel<LogLine>>, text: String, error: bool) {
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
    std::thread::spawn(move || {
        let result = install_all(&cfg, backend, opencl, &tx);
        match result {
            Ok(()) => {
                let _ = tx.send(UiEvent::JobDone("install"));
            }
            Err(error) => {
                let _ = tx.send(UiEvent::JobFailed(format!("{error:#}")));
            }
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

    let registry = fs::read_to_string(prefix.join("system.reg")).unwrap_or_default();
    if !wine::has_dotnet48(&registry) {
        anyhow::bail!(".NET Framework 4.8 is not registered in the prefix");
    }

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
        format!("{found}/{} found, all required present", report.deps.len())
    } else {
        format!(
            "{found}/{} found, missing: {}",
            report.deps.len(),
            missing.join(", ")
        )
    };
    let wine = report
        .wine_build
        .clone()
        .unwrap_or_else(|| "not installed".into());
    let prefix = format!(
        "{} (exists: {}, installed: {})",
        report.prefix.display(),
        report.prefix_exists,
        report.installed
    );

    ui.set_distro_text(distro.clone().into());
    ui.set_gpu_text(gpu.clone().into());
    ui.set_deps_text(deps.clone().into());
    ui.set_wine_text(wine.clone().into());
    ui.set_prefix_text(prefix.clone().into());
    ui.set_installed(report.installed);
    ui.set_busy(false);

    let state = if report.installed {
        "Installed. Use Launch Affinity to start it."
    } else {
        "Not installed. Install Affinity runs dependencies, Wine, GPU and the app."
    };
    ui.set_state_text(state.into());

    push_log(logs, format!("distro: {distro}"), false);
    push_log(logs, format!("gpu: {gpu}"), false);
    push_log(logs, format!("wine: {wine}"), false);
    push_log(logs, format!("deps: {deps}"), !missing.is_empty());
    push_log(logs, format!("prefix: {prefix}"), false);
    for error in &report.errors {
        push_log(logs, format!("error: {error}"), true);
    }
}
