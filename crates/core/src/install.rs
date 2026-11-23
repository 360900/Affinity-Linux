//! End-to-end Affinity install, update and repair.
//!
//! The pipeline is: host dependencies, optional OpenCL driver, patched Wine
//! with .NET 4.8, GPU backend (vkd3d-proton or DXVK), Affinity's setup
//! executable and post-install extras. Each stage records its result in the
//! prefix manifest.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

use crate::config::GpuBackend;
use crate::distro::OsRelease;
use crate::journal::Report;
use crate::manifest::{
    Manifest, STEP_BACKENDS, STEP_DEPS, STEP_DESKTOP, STEP_FACADES, STEP_INSTALLER, STEP_PATCH,
    STEP_PLUGINLOADER, STEP_PREFIX, STEP_SETTINGS, STEP_WEBVIEW2, STEP_WINE, STEP_WINETRICKS,
    StepStatus,
};
use crate::packages::{Elevation, PlanRunOptions};
use crate::runner::Stream;
use crate::{app_exe, backends, deps, desktop, extras, gpu, installer, packages, patch, wine};

/// Steps run inside this module stream through its report, which already
/// writes the log; giving them the path too doubled every line.
const CHILD_LOG: Option<PathBuf> = None;

#[derive(Debug, Clone)]
pub struct SetupOptions {
    pub prefix: PathBuf,
    pub installer: Option<PathBuf>,
    pub backend: GpuBackend,
    pub opencl: bool,
    pub dpi: Option<u32>,
    pub dry_run: bool,
    pub log_path: Option<PathBuf>,
    /// Install missing host packages first.
    pub install_deps: bool,
    /// Reuse the existing Wine prefix instead of running wine-setup.
    pub skip_wine: bool,
    pub elevation: Elevation,
    /// Optional distro override from Config, mainly for dry-run testing.
    pub distro_override: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SetupReport {
    pub wine_version: Option<String>,
    pub vkd3d_version: Option<String>,
    pub dxvk_version: Option<String>,
    pub used_setup_ui: bool,
    pub facades: bool,
    pub patched: bool,
    pub pluginloader: bool,
    pub settings: bool,
    pub webview2: bool,
    pub url_handler: bool,
    pub failures: Vec<String>,
}

pub fn install<F>(opts: &SetupOptions, on_line: F) -> Result<SetupReport>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let mut outcome = SetupReport::default();

    crate::config::validate_prefix(&opts.prefix)?;
    report.info("Affinity v3 installation");

    let os = OsRelease::detect_with_override(opts.distro_override.as_deref())?;

    if opts.install_deps {
        if !install_dependencies(opts, &os, &mut report)? {
            outcome.failures.push("host dependencies".to_string());
        }
        if opts.opencl {
            install_opencl(opts, &os, &mut report);
        }
    } else {
        report.info("host dependencies: skipped");
        mark(
            &opts.prefix,
            STEP_DEPS,
            StepStatus::Skipped,
            None,
            opts.dry_run,
        );
    }

    if !opts.skip_wine {
        let setup = wine::setup(
            &wine::SetupOptions {
                prefix: opts.prefix.clone(),
                family: os.family(),
                dry_run: opts.dry_run,
                log_path: CHILD_LOG,
                wine_version: None,
                dpi: opts.dpi,
            },
            |stream, line| report.line(stream, line),
        )?;
        outcome.wine_version = setup.wine_version;
        for component in &setup.failed_components {
            outcome
                .failures
                .push(format!("winetricks component: {component}"));
        }
        mark(
            &opts.prefix,
            STEP_PREFIX,
            StepStatus::Done,
            None,
            opts.dry_run,
        );
        mark(
            &opts.prefix,
            STEP_WINE,
            StepStatus::Done,
            outcome.wine_version.clone(),
            opts.dry_run,
        );
        mark(
            &opts.prefix,
            STEP_WINETRICKS,
            if setup.failed_components.is_empty() {
                StepStatus::Done
            } else {
                StepStatus::Failed
            },
            None,
            opts.dry_run,
        );
    }

    if !opts.dry_run {
        let registry = fs::read_to_string(opts.prefix.join("system.reg")).unwrap_or_default();
        if !wine::has_dotnet48(&registry) {
            bail!(
                ".NET Framework 4.8 is not registered in {}; run `affinity-linux wine-setup` first",
                opts.prefix.display()
            );
        }
    }

    apply_backend(opts, &mut report, &mut outcome);

    mark(
        &opts.prefix,
        STEP_INSTALLER,
        StepStatus::Running,
        None,
        opts.dry_run,
    );
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
    mark(
        &opts.prefix,
        STEP_INSTALLER,
        StepStatus::Done,
        None,
        opts.dry_run,
    );

    run_extras(opts, &mut report, &mut outcome)?;
    finish(opts, &mut report, &outcome);
    Ok(outcome)
}

/// Reinstall the latest Affinity build and re-apply post-install fixes.
pub fn update<F>(opts: &SetupOptions, on_line: F) -> Result<SetupReport>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let mut outcome = SetupReport::default();

    crate::config::validate_prefix(&opts.prefix)?;
    report.info("Affinity v3 update");
    if !opts.dry_run {
        ensure_installed(&opts.prefix)?;
    }

    mark(
        &opts.prefix,
        STEP_INSTALLER,
        StepStatus::Running,
        None,
        opts.dry_run,
    );
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
    mark(
        &opts.prefix,
        STEP_INSTALLER,
        StepStatus::Done,
        None,
        opts.dry_run,
    );

    run_extras(opts, &mut report, &mut outcome)?;
    finish(opts, &mut report, &outcome);
    Ok(outcome)
}

/// Re-apply DPI, GPU backend, OpenCL driver and post-install fixes without
/// reinstalling Affinity.
pub fn repair<F>(opts: &SetupOptions, on_line: F) -> Result<SetupReport>
where
    F: FnMut(Stream, &str),
{
    let mut sink = on_line;
    let mut report = Report::new(opts.log_path.clone(), &mut sink);
    let mut outcome = SetupReport::default();

    crate::config::validate_prefix(&opts.prefix)?;
    report.info("Affinity v3 repair");
    if !opts.dry_run {
        ensure_installed(&opts.prefix)?;
    }

    if let Some(dpi) = opts.dpi
        && let Err(error) = wine::apply_dpi(
            &opts.prefix,
            dpi,
            opts.dry_run,
            CHILD_LOG,
            |stream, line| report.line(stream, line),
        )
    {
        report.warn(format!("could not set the Wine DPI: {error:#}"));
    }

    apply_backend(opts, &mut report, &mut outcome);
    run_extras(opts, &mut report, &mut outcome)?;

    if opts.opencl
        && let Ok(os) = OsRelease::detect_with_override(opts.distro_override.as_deref())
    {
        install_opencl(opts, &os, &mut report);
    }

    finish(opts, &mut report, &outcome);
    Ok(outcome)
}

pub fn ensure_installed(prefix: &Path) -> Result<()> {
    if !app_exe(prefix).exists() {
        bail!(
            "Affinity is not installed in {}; run the installer again",
            prefix.display()
        );
    }
    let registry = fs::read_to_string(prefix.join("system.reg")).unwrap_or_default();
    if !wine::has_dotnet48(&registry) {
        bail!(
            ".NET Framework 4.8 is missing from {}; reinstall Affinity",
            prefix.display()
        );
    }
    Ok(())
}

/// Downloads the Affinity setup (unless the user brought one) and WebView2
/// into the shared cache. Meant for a background thread started with the
/// install, so the downloads overlap Wine setup; `install` then finds them
/// cached. Failures only warn: `install` downloads again if needed.
pub fn prefetch<F>(log_path: Option<PathBuf>, with_installer: bool, on_line: F)
where
    F: FnMut(Stream, &str),
{
    prefetch_cancellable(log_path, with_installer, None, on_line);
}

/// Like `prefetch`, but stops promptly when `cancel` is set.
pub fn prefetch_cancellable<F>(
    log_path: Option<PathBuf>,
    with_installer: bool,
    cancel: Option<Arc<AtomicBool>>,
    on_line: F,
) where
    F: FnMut(Stream, &str),
{
    if deps::which("curl").is_none() && deps::which("wget").is_none() {
        return;
    }
    let mut sink = on_line;
    let mut report = Report::new(log_path, &mut sink);
    report.info("downloading Affinity and WebView2 in the background");
    if with_installer && let Err(error) = installer::fetch_installer(&mut report, cancel.clone()) {
        if cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return;
        }
        report.warn(format!("background download failed: {error:#}"));
    }
    if cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        return;
    }
    if let Err(error) = extras::fetch_webview2(&mut report, cancel.clone()) {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return;
        }
        report.warn(format!("background download failed: {error:#}"));
    }
}

pub fn format_missing_deps(missing: &[&deps::DepCheck], has_download: bool) -> String {
    let names: Vec<&str> = missing.iter().map(|dep| dep.command).collect();
    if names.is_empty() && !has_download {
        "curl or wget".to_string()
    } else if !has_download {
        format!("{} and curl or wget", names.join(", "))
    } else {
        names.join(", ")
    }
}

fn install_dependencies(
    opts: &SetupOptions,
    os: &OsRelease,
    report: &mut Report<'_>,
) -> Result<bool> {
    let checks = deps::check_all();
    let missing = deps::missing(&checks);
    let has_download = deps::has_download_tool(&checks);
    if !opts.dry_run && missing.is_empty() && has_download {
        report.info("dependencies are already present");
        mark(
            &opts.prefix,
            STEP_DEPS,
            StepStatus::Done,
            None,
            opts.dry_run,
        );
        return Ok(true);
    }

    let plan = packages::plan(os);
    for note in &plan.notes {
        report.info(format!("note: {note}"));
    }
    if plan.steps.is_empty() {
        if opts.dry_run {
            return Ok(true);
        }
        let detail = format_missing_deps(&missing, has_download);
        report.warn(format!(
            "no automatic dependency plan for this distribution; missing: {detail}"
        ));
        mark(
            &opts.prefix,
            STEP_DEPS,
            StepStatus::Failed,
            Some(detail),
            opts.dry_run,
        );
        return Ok(false);
    }

    if !opts.dry_run {
        if opts.elevation == Elevation::Sudo && deps::which("sudo").is_none() {
            bail!("sudo is missing; install the dependencies manually or run as root");
        }
        if opts.elevation == Elevation::Pkexec && deps::which("pkexec").is_none() {
            bail!("pkexec is missing; install the dependencies manually or run as root");
        }
    }

    let options = PlanRunOptions {
        dry_run: opts.dry_run,
        elevation: opts.elevation,
        log_path: None,
    };
    let outcome = packages::run_plan(&plan, &options, |stream, line| report.line(stream, line))?;
    if !outcome.failed_optional.is_empty() {
        report.warn(format!(
            "optional dependency steps failed: {}",
            outcome.failed_optional.join(", ")
        ));
    }
    mark(
        &opts.prefix,
        STEP_DEPS,
        StepStatus::Done,
        None,
        opts.dry_run,
    );
    Ok(true)
}

fn install_opencl(opts: &SetupOptions, os: &OsRelease, report: &mut Report<'_>) {
    let plan = packages::opencl_plan(os, gpu::render_vendor(&opts.prefix));
    for note in &plan.notes {
        report.info(format!("OpenCL: {note}"));
    }
    if plan.steps.is_empty() {
        return;
    }
    if !opts.dry_run {
        if opts.elevation == Elevation::Sudo && deps::which("sudo").is_none() {
            report.warn("sudo is missing; skipping the OpenCL driver");
            return;
        }
        if opts.elevation == Elevation::Pkexec && deps::which("pkexec").is_none() {
            report.warn("pkexec is missing; skipping the OpenCL driver");
            return;
        }
    }
    let options = PlanRunOptions {
        dry_run: opts.dry_run,
        elevation: opts.elevation,
        log_path: None,
    };
    match packages::run_plan(&plan, &options, |stream, line| report.line(stream, line)) {
        Ok(outcome) if !outcome.failed_optional.is_empty() => {
            report.warn(format!(
                "optional OpenCL steps failed: {}",
                outcome.failed_optional.join(", ")
            ));
        }
        Ok(_) => {}
        Err(error) => report.warn(format!("OpenCL driver install failed: {error:#}")),
    }
}

fn apply_backend(opts: &SetupOptions, report: &mut Report<'_>, outcome: &mut SetupReport) {
    let backend_options = backends::BackendOptions {
        prefix: opts.prefix.clone(),
        backend: opts.backend,
        dry_run: opts.dry_run,
        log_path: CHILD_LOG,
    };
    match backends::install(&backend_options, |stream, line| report.line(stream, line)) {
        Ok(result) => {
            outcome.vkd3d_version = result.vkd3d_version;
            outcome.dxvk_version = result.dxvk_version;
            mark(
                &opts.prefix,
                STEP_BACKENDS,
                StepStatus::Done,
                None,
                opts.dry_run,
            );
        }
        Err(error) => {
            report.warn(format!("GPU backend setup failed: {error:#}"));
            mark(
                &opts.prefix,
                STEP_BACKENDS,
                StepStatus::Failed,
                Some(error.to_string()),
                opts.dry_run,
            );
            outcome.failures.push(format!("GPU backend: {error}"));
        }
    }
}

fn run_extras(
    opts: &SetupOptions,
    report: &mut Report<'_>,
    outcome: &mut SetupReport,
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
                mark(
                    &opts.prefix,
                    STEP_PATCH,
                    status,
                    result.skipped,
                    opts.dry_run,
                );
            }
            Err(error) => {
                report.warn(format!("settings patch failed: {error}"));
                mark(
                    &opts.prefix,
                    STEP_PATCH,
                    StepStatus::Failed,
                    Some(error.to_string()),
                    opts.dry_run,
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
            mark(
                &opts.prefix,
                STEP_DESKTOP,
                StepStatus::Done,
                None,
                opts.dry_run,
            );
        }
        Err(error) => {
            report.warn(format!("desktop entry failed: {error}"));
            mark(
                &opts.prefix,
                STEP_DESKTOP,
                StepStatus::Failed,
                Some(error.to_string()),
                opts.dry_run,
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

fn finish(opts: &SetupOptions, report: &mut Report<'_>, outcome: &SetupReport) {
    if opts.dry_run {
        return;
    }
    if let Ok(mut manifest) = Manifest::load_or_new(&opts.prefix) {
        manifest.affinity_exe = app_exe(&opts.prefix)
            .exists()
            .then(|| app_exe(&opts.prefix).display().to_string());
        if outcome.wine_version.is_some() {
            manifest.wine_version = outcome.wine_version.clone();
        }
        let _ = manifest.save(&opts.prefix);
    }
    report.info(format!(
        "finished with {} failure(s)",
        outcome.failures.len()
    ));
}

fn record(
    report: &mut Report<'_>,
    outcome: &mut SetupReport,
    step: &str,
    result: Result<bool>,
    opts: &SetupOptions,
) -> bool {
    match result {
        Ok(true) => {
            mark(&opts.prefix, step, StepStatus::Done, None, opts.dry_run);
            true
        }
        Ok(false) => {
            mark(&opts.prefix, step, StepStatus::Skipped, None, opts.dry_run);
            false
        }
        Err(error) => {
            report.warn(format!("{step} failed: {error}"));
            mark(
                &opts.prefix,
                step,
                StepStatus::Failed,
                Some(error.to_string()),
                opts.dry_run,
            );
            outcome.failures.push(format!("{step}: {error}"));
            false
        }
    }
}

fn mark(prefix: &Path, step: &str, status: StepStatus, detail: Option<String>, dry_run: bool) {
    if dry_run {
        return;
    }
    if let Ok(mut manifest) = Manifest::load_or_new(prefix) {
        manifest.mark(step, status, detail);
        let _ = manifest.save(prefix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(prefix: &Path) -> SetupOptions {
        SetupOptions {
            prefix: prefix.to_path_buf(),
            installer: None,
            backend: GpuBackend::Vkd3d,
            opencl: false,
            dpi: None,
            dry_run: true,
            log_path: None,
            install_deps: false,
            skip_wine: false,
            elevation: Elevation::None,
            distro_override: Some("arch".into()),
        }
    }

    #[test]
    fn dry_run_covers_the_whole_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        let mut lines = Vec::new();
        let report = install(&options(&prefix), |_, line| lines.push(line.to_string())).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("ElementalWarrior-wine-11.19.tar.xz"));
        assert!(text.contains("Affinity-x64.exe"));
        assert!(text.contains("WinRT facades"));
        assert!(text.contains("AffinityPatcher"));
        assert!(text.contains("AffinityPluginLoader"));
        assert!(text.contains("settings"));
        assert!(text.contains("WebView2"));
        assert!(text.contains("desktop entry"));
        assert!(report.failures.is_empty());
        assert!(!prefix.exists());
    }

    #[test]
    fn repair_dry_run_skips_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        let mut lines = Vec::new();
        repair(&options(&prefix), |_, line| lines.push(line.to_string())).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("repair"));
        assert!(text.contains("WinRT facades"));
        assert!(text.contains("desktop entry"));
        assert!(!text.contains("Affinity-x64.exe"));
    }
}
