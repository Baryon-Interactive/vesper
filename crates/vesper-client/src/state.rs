use crate::{install::InstallRequest, settings::Settings, tasks::TaskRecord};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
};

/// Shared settings and in-memory task controls for client operations.
#[derive(Default)]
pub struct AppState {
    pub(crate) settings: std::sync::Arc<Mutex<Settings>>,
    pub(crate) tasks: std::sync::Arc<Mutex<BTreeMap<String, TaskRecord>>>,
    pub(crate) requests: std::sync::Arc<Mutex<BTreeMap<String, InstallRequest>>>,
    pub(crate) controls: std::sync::Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            tasks: self.tasks.clone(),
            requests: self.requests.clone(),
            controls: self.controls.clone(),
        }
    }
}

/// Construct shared client state with the supplied settings.
pub fn new_app_state(settings: Settings) -> AppState {
    AppState {
        settings: Arc::new(Mutex::new(settings)),
        tasks: Arc::new(Mutex::new(BTreeMap::new())),
        requests: Arc::new(Mutex::new(BTreeMap::new())),
        controls: Arc::new(Mutex::new(BTreeMap::new())),
    }
}

pub(crate) fn state_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("VESPER_STATE_DIR") {
        return PathBuf::from(path);
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("Vesper"))
        .join("Vesper")
}

pub(crate) fn settings_path() -> PathBuf {
    state_dir().join("settings.json")
}

pub(crate) fn task_path(id: &str) -> PathBuf {
    state_dir().join("tasks").join(format!("{id}.json"))
}

pub(crate) fn installed_targets_path(root: &Path) -> PathBuf {
    root.join(".vesper/installed-targets.json")
}
