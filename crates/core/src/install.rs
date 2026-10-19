//! End-to-end Affinity install: setup executable plus post-install extras.

use std::path::PathBuf;

use anyhow::Result;

use crate::journal::Report;
use crate::manifest::{
    Manifest, STEP_DESKTOP, STEP_FACADES, STEP_INSTALLER, STEP_PATCH, STEP_PLUGINLOADER,
    STEP_SETTINGS, STEP_WEBVIEW2, StepStatus,
};
use crate::runner::Stream;
use crate::{app_exe, desktop, extras, installer, patch};

/// Steps run inside `install`/`repair` stream through this module's report,
/// which already writes the log; giving them the path too doubled every line.
const CHILD_LOG: Option<PathBuf> = None;

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub prefix: PathBuf,
    pub installer: Option<PathBuf>,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct InstallReport {
    pub used_setup_ui: bool,
    pub facades: bool,
    pub patched: bool,
    pub pluginloader: bool,
    pub settings: bool,
    pub webview2: bool,
    pub url_handler: bool,
    pub failures: Vec<String>,
}

pub fn install<F>(opts: &InstallOptions, on_line: F) -> Result<InstallReport>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let mut outcome = InstallReport::default();

    report.info("Affinity v3 installation");
    mark(opts, STEP_INSTALLER, StepStatus::Running, None);
    let setup = installer::install(
        &installer::InstallerOptions {
            prefix: opts.prefix.clone(),
            installer: opts.installer.clone(),
            dry_run: opts.dry_run,
            log_path: CHILD_LOG,
        },
        |stream, line| report.line(stream, line),
    )?;
    outcome.used_setup_ui = setup.used_setup_ui;
    mark(opts, STEP_INSTALLER, StepStatus::Done, None);

    run_extras(opts, &mut report, &mut outcome)?;
    finish(opts, &mut report, &outcome);
    Ok(outcome)
}

/// Downloads the Affinity setup (unless the user brought one) and WebView2
/// into the shared cache. Meant for a background thread started with the
/// install, so the downloads overlap Wine setup; `install` then finds them
/// cached. Failures only warn: `install` downloads again if needed.
pub fn prefetch<F>(log_path: Option<PathBuf>, with_installer: bool, on_line: F)
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);
    report.info("downloading Affinity and WebView2 in the background");
    if with_installer && let Err(error) = installer::fetch_installer(&mut report) {
        report.warn(format!("background download failed: {error:#}"));
    }
    if let Err(error) = extras::fetch_webview2(&mut report) {
        report.warn(format!("background download failed: {error:#}"));
    }
}

/// Re-apply post-install fixes without running the Affinity installer again.
pub fn repair<F>(opts: &InstallOptions, on_line: F) -> Result<InstallReport>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let mut outcome = InstallReport::default();

    report.info("Affinity v3 repair");
    run_extras(opts, &mut report, &mut outcome)?;
    finish(opts, &mut report, &outcome);
    Ok(outcome)
}

fn run_extras(
    opts: &InstallOptions,
    report: &mut Report<'_>,
    outcome: &mut InstallReport,
) -> Result<()> {
    let facades = extras::install_facades(&opts.prefix, opts.dry_run, CHILD_LOG, |stream, line| {
        report.line(stream, line)
    });
    outcome.facades = record(report, outcome, STEP_FACADES, facades, opts);

    if opts.dry_run || app_exe(&opts.prefix).exists() {
        let patched = patch::apply(&opts.prefix, opts.dry_run, CHILD_LOG, |stream, line| {
            report.line(stream, line)
        });
        match patched {
            Ok(result) => {
                outcome.patched = result.patched;
                let status = if result.patched {
                    StepStatus::Done
                } else {
                    StepStatus::Skipped
                };
                mark(opts, STEP_PATCH, status, result.skipped);
            }
            Err(error) => {
                report.warn(format!("settings patch failed: {error}"));
                mark(
                    opts,
                    STEP_PATCH,
                    StepStatus::Failed,
                    Some(error.to_string()),
                );
                outcome.failures.push(format!("patch: {error}"));
            }
        }
    }

    let loader =
        extras::install_plugin_loader(&opts.prefix, opts.dry_run, CHILD_LOG, |stream, line| {
            report.line(stream, line)
        });
    outcome.pluginloader = record(report, outcome, STEP_PLUGINLOADER, loader, opts);

    let desktop = desktop::create(
        &desktop::DesktopOptions {
            prefix: opts.prefix.clone(),
            dry_run: opts.dry_run,
            log_path: CHILD_LOG,
        },
        |stream, line| report.line(stream, line),
    );
    match desktop {
        Ok(result) => {
            outcome.url_handler = result.url_handler;
            mark(opts, STEP_DESKTOP, StepStatus::Done, None);
        }
        Err(error) => {
            report.warn(format!("desktop entry failed: {error}"));
            mark(
                opts,
                STEP_DESKTOP,
                StepStatus::Failed,
                Some(error.to_string()),
            );
            outcome.failures.push(format!("desktop: {error}"));
        }
    }

    let settings = extras::seed_settings(&opts.prefix, opts.dry_run, CHILD_LOG, |stream, line| {
        report.line(stream, line)
    });
    outcome.settings = record(report, outcome, STEP_SETTINGS, settings, opts);

    let webview2 =
        extras::install_webview2(&opts.prefix, opts.dry_run, CHILD_LOG, |stream, line| {
            report.line(stream, line)
        });
    outcome.webview2 = record(report, outcome, STEP_WEBVIEW2, webview2, opts);

    Ok(())
}

fn finish(opts: &InstallOptions, report: &mut Report<'_>, outcome: &InstallReport) {
    if opts.dry_run {
        return;
    }
    if let Ok(mut manifest) = Manifest::load_or_new(&opts.prefix) {
        manifest.affinity_exe = app_exe(&opts.prefix)
            .exists()
            .then(|| app_exe(&opts.prefix).display().to_string());
        let _ = manifest.save(&opts.prefix);
    }
    report.info(format!(
        "finished with {} failure(s)",
        outcome.failures.len()
    ));
}

fn record(
    report: &mut Report<'_>,
    outcome: &mut InstallReport,
    step: &str,
    result: Result<bool>,
    opts: &InstallOptions,
) -> bool {
    match result {
        Ok(true) => {
            mark(opts, step, StepStatus::Done, None);
            true
        }
        Ok(false) => {
            mark(opts, step, StepStatus::Skipped, None);
            false
        }
        Err(error) => {
            report.warn(format!("{step} failed: {error}"));
            mark(opts, step, StepStatus::Failed, Some(error.to_string()));
            outcome.failures.push(format!("{step}: {error}"));
            false
        }
    }
}

fn mark(opts: &InstallOptions, step: &str, status: StepStatus, detail: Option<String>) {
    if opts.dry_run {
        return;
    }
    if let Ok(mut manifest) = Manifest::load_or_new(&opts.prefix) {
        manifest.mark(step, status, detail);
        let _ = manifest.save(&opts.prefix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_covers_the_whole_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let options = InstallOptions {
            prefix: dir.path().to_path_buf(),
            installer: None,
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        let report = install(&options, |_, line| lines.push(line.to_string())).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("Affinity-x64.exe"));
        assert!(text.contains("WinRT facades"));
        assert!(text.contains("AffinityPatcher"));
        assert!(text.contains("AffinityPluginLoader"));
        assert!(text.contains("settings"));
        assert!(text.contains("WebView2"));
        assert!(text.contains("desktop entry"));
        assert!(report.failures.is_empty());
        assert!(!dir.path().join("Installer").exists());
    }

    #[test]
    fn repair_dry_run_skips_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let options = InstallOptions {
            prefix: dir.path().to_path_buf(),
            installer: None,
            dry_run: true,
            log_path: None,
        };
        let mut lines = Vec::new();
        repair(&options, |_, line| lines.push(line.to_string())).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("repair"));
        assert!(text.contains("WinRT facades"));
        assert!(text.contains("desktop entry"));
        assert!(!text.contains("Affinity-x64.exe"));
    }
}
