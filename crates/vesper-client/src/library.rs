use crate::state::AppState;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
/// An installed application discovered from its control directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledGame {
    /// Published application identifier.
    pub app_id: String,
    /// Installed game version recorded in the control directory.
    pub version: String,
    /// Version shown to the user.
    pub display_version: String,
    /// Selected published release channel.
    pub channel: String,
    /// Installed size excluding control files.
    pub size_bytes: u64,
    /// Task or installation status string.
    pub status: String,
    /// Game installation directory.
    pub install_dir: String,
    /// Whether rollback backup files are available.
    pub has_backup: bool,
    /// User-file paths reported for the installation.
    pub user_files: Vec<String>,
}
fn size(root: &Path) -> u64 {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let path = entry.path();
                    if path.file_name().and_then(|v| v.to_str()) == Some(".vesper") {
                        0
                    } else if entry.file_type().map(|v| v.is_dir()).unwrap_or(false) {
                        size(&path)
                    } else {
                        entry.metadata().map(|v| v.len()).unwrap_or(0)
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Check whether an installation has backup files available for rollback.
pub fn has_recoverable_backup(control: &Path) -> bool {
    let Ok(transactions) = control.join("backup").read_dir() else {
        return false;
    };
    transactions.flatten().any(|transaction| {
        transaction
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
            && walk_has_file(&transaction.path())
    })
}

fn walk_has_file(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_type()
            .map(|kind| {
                if kind.is_file() {
                    true
                } else if kind.is_dir() {
                    walk_has_file(&entry.path())
                } else {
                    false
                }
            })
            .unwrap_or(false)
    })
}
/// Discover installed applications in the configured library directory.
pub fn scan(library_dir: &Path) -> Result<Vec<InstalledGame>> {
    let mut result = Vec::new();
    if !library_dir.is_dir() {
        return Ok(result);
    };
    for entry in fs::read_dir(library_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        };
        let root = entry.path();
        let control = control_path(&root);
        if !control.join("installed-manifest.cbor.zst").is_file() {
            continue;
        };
        let app_id = root
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .to_owned();
        let version = fs::read_to_string(control.join("current"))
            .ok()
            .and_then(|v| v.lines().next().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        let channel = fs::read_to_string(control.join("channel"))
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "stable".into());
        let has_backup = has_recoverable_backup(&control);
        result.push(InstalledGame {
            app_id,
            display_version: version.clone(),
            version,
            channel,
            size_bytes: size(&root),
            status: "installed".into(),
            install_dir: root.to_string_lossy().into_owned(),
            has_backup,
            user_files: Vec::new(),
        });
    }
    result.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    Ok(result)
}
/// Return the control directory path for an installation.
pub fn control_path(root: impl Into<PathBuf>) -> PathBuf {
    root.into().join(".vesper")
}

/// List installed games in the configured library directory.
pub fn list_installed(state: &AppState) -> Result<Vec<InstalledGame>> {
    let settings = state.settings.lock().unwrap().clone();
    scan(Path::new(&settings.library_dir))
}
