use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const MANIFEST_FILE: &str = "install-manifest.json";

// Install steps, in pipeline order.
pub const STEP_PREFIX: &str = "prefix";
pub const STEP_DEPS: &str = "deps";
pub const STEP_WINE: &str = "wine";
pub const STEP_WINETRICKS: &str = "winetricks";
pub const STEP_BACKENDS: &str = "backends";
pub const STEP_INSTALLER: &str = "installer";
pub const STEP_PATCH: &str = "patch";
pub const STEP_SETTINGS: &str = "settings";
pub const STEP_PLUGINLOADER: &str = "pluginloader";
pub const STEP_FACADES: &str = "facades";
pub const STEP_WEBVIEW2: &str = "webview2";
pub const STEP_DESKTOP: &str = "desktop-entry";

pub const ALL_STEPS: &[&str] = &[
    STEP_PREFIX,
    STEP_DEPS,
    STEP_WINE,
    STEP_WINETRICKS,
    STEP_BACKENDS,
    STEP_INSTALLER,
    STEP_PATCH,
    STEP_SETTINGS,
    STEP_PLUGINLOADER,
    STEP_FACADES,
    STEP_WEBVIEW2,
    STEP_DESKTOP,
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub status: StepStatus,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub app: String,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wine_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity_exe: Option<String>,
    #[serde(default)]
    pub steps: BTreeMap<String, StepRecord>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            app: "affinity-v3".to_string(),
            created_at: now_epoch(),
            updated_at: now_epoch(),
            wine_version: None,
            affinity_exe: None,
            steps: BTreeMap::new(),
        }
    }
}

impl Manifest {
    pub fn path(prefix: &Path) -> PathBuf {
        prefix.join(MANIFEST_FILE)
    }

    pub fn load(prefix: &Path) -> Result<Option<Self>> {
        let path = Self::path(prefix);
        if !path.exists() {
            return Ok(None);
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let manifest =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(manifest))
    }

    pub fn load_or_new(prefix: &Path) -> Result<Self> {
        Ok(Self::load(prefix)?.unwrap_or_default())
    }

    pub fn save(&mut self, prefix: &Path) -> Result<()> {
        self.updated_at = now_epoch();
        fs::create_dir_all(prefix).with_context(|| format!("creating {}", prefix.display()))?;
        let path = Self::path(prefix);
        let text = serde_json::to_string_pretty(self).context("serializing manifest")?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn mark(&mut self, step: &str, status: StepStatus, detail: Option<String>) {
        self.steps.insert(
            step.to_string(),
            StepRecord {
                status,
                at: now_epoch(),
                detail,
            },
        );
    }

    pub fn status_of(&self, step: &str) -> StepStatus {
        self.steps.get(step).map(|r| r.status).unwrap_or_default()
    }

    pub fn is_done(&self, step: &str) -> bool {
        self.status_of(step) == StepStatus::Done
    }

    /// Steps that must be redone because they never completed.
    pub fn incomplete_steps(&self) -> Vec<&'static str> {
        ALL_STEPS
            .iter()
            .copied()
            .filter(|step| self.status_of(step) != StepStatus::Done)
            .collect()
    }
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_mark() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();

        let mut manifest = Manifest::load_or_new(prefix).unwrap();
        assert!(manifest.steps.is_empty());
        assert_eq!(manifest.incomplete_steps().len(), ALL_STEPS.len());

        manifest.mark(STEP_WINE, StepStatus::Done, Some("11.19".into()));
        manifest.affinity_exe = Some("/x/Affinity.exe".into());
        manifest.save(prefix).unwrap();

        let loaded = Manifest::load(prefix).unwrap().unwrap();
        assert_eq!(loaded.schema, SCHEMA);
        assert!(loaded.is_done(STEP_WINE));
        assert_eq!(loaded.incomplete_steps().len(), ALL_STEPS.len() - 1);
        assert_eq!(loaded.affinity_exe.as_deref(), Some("/x/Affinity.exe"));
    }

    #[test]
    fn missing_manifest_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Manifest::load(dir.path()).unwrap().is_none());
    }
}
