use crate::state::{settings_path, state_dir, AppState};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

/// Persisted client settings with the launcher defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Directory containing installed application directories.
    pub library_dir: String,
    /// Static index API base URL.
    pub api_base: String,
    /// Account and paid-key gateway base URL.
    pub gateway_base: String,
    /// Configured download concurrency.
    pub concurrency: usize,
    /// Configured bandwidth limit in KiB per second.
    pub bandwidth_limit_kib: u64,
    /// Enabled source identifiers or source kinds.
    pub enabled_sources: Vec<String>,
    /// Configured launcher updater endpoint.
    pub updater_endpoint: String,
}

impl Default for Settings {
    fn default() -> Self {
        let root = state_dir();
        Self {
            library_dir: root.join("library").to_string_lossy().into_owned(),
            api_base: "https://baryon-interactive.github.io/vesper-index".into(),
            gateway_base: String::new(),
            concurrency: 8,
            bandwidth_limit_kib: 0,
            enabled_sources: vec!["github".into(), "http".into(), "hf".into(), "r2".into()],
            updater_endpoint: String::new(),
        }
    }
}

/// Clone the current in-memory client settings.
pub fn get_settings(state: &AppState) -> Result<Settings> {
    Ok(state.settings.lock().unwrap().clone())
}

/// Persist settings in the JSON format and update shared state.
pub fn save_settings(settings: Settings, state: &AppState) -> Result<()> {
    fs::create_dir_all(state_dir())?;
    fs::write(settings_path(), serde_json::to_vec_pretty(&settings)?)?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

/// Load persisted settings or return the defaults.
pub fn load_settings() -> Settings {
    fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
