slint::include_modules!();

use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use affinity_linux_core::config::Config;
use affinity_linux_core::doctor::{self, DoctorReport};
use slint::{ComponentHandle, VecModel};

enum UiEvent {
    Doctor(Box<DoctorReport>),
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

    let (tx, rx) = mpsc::channel::<UiEvent>();
    spawn_doctor(cfg.clone(), tx.clone());

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_install_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_state_text("Install engine lands in M2-M4".into());
            }
            logs.push(LogLine {
                text: "install: not implemented yet (M2-M4)".into(),
                error: true,
            });
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_launch_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_state_text("Launcher lands in M5".into());
            }
            logs.push(LogLine {
                text: "launch: not implemented yet (M5)".into(),
                error: true,
            });
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_uninstall_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_state_text("Uninstall lands in M5".into());
            }
            logs.push(LogLine {
                text: "uninstall: not implemented yet (M5)".into(),
                error: true,
            });
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
                }
            }
        },
    );

    ui.run()
}

fn spawn_doctor(cfg: Config, tx: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let report = doctor::run(&cfg);
        let _ = tx.send(UiEvent::Doctor(Box::new(report)));
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
        "Installed. Launch engine lands in M5."
    } else {
        "Not installed. Install engine lands in M2-M4."
    };
    ui.set_state_text(state.into());

    logs.push(LogLine {
        text: format!("distro: {distro}").into(),
        error: false,
    });
    logs.push(LogLine {
        text: format!("gpu: {gpu}").into(),
        error: false,
    });
    logs.push(LogLine {
        text: format!("wine: {wine}").into(),
        error: false,
    });
    logs.push(LogLine {
        text: format!("deps: {deps}").into(),
        error: !missing.is_empty(),
    });
    logs.push(LogLine {
        text: format!("prefix: {prefix}").into(),
        error: false,
    });
    for error in &report.errors {
        logs.push(LogLine {
            text: format!("error: {error}").into(),
            error: true,
        });
    }
}
