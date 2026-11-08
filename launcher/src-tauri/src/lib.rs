//! Tauri event adapter for the window-independent Vesper client.

use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
pub use vesper_client::*;

/// Forward client events to the frontend without changing their names or payloads.
pub struct TauriEventSink(pub AppHandle);

impl EventSink for TauriEventSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
}

/// Start an installation and forward its worker events to Tauri.
pub fn start_install(request: InstallRequest, app: &AppHandle, state: &AppState) -> Result<String> {
    vesper_client::start_install(request, Arc::new(TauriEventSink(app.clone())), state)
}

/// Pause a task and forward its status to Tauri.
pub fn pause_task(task_id: &str, app: &AppHandle, state: &AppState) -> Result<()> {
    vesper_client::pause_task(task_id, &TauriEventSink(app.clone()), state)
}

/// Resume a task with a Tauri event sink for the worker.
pub fn resume_task(task_id: &str, app: &AppHandle, state: &AppState) -> Result<()> {
    vesper_client::resume_task(task_id, Arc::new(TauriEventSink(app.clone())), state)
}

/// Repair an installed game and forward completion to Tauri.
pub fn repair_game(install_dir: &str, state: &AppState, app: &AppHandle) -> Result<String> {
    vesper_client::repair_game(install_dir, state, &TauriEventSink(app.clone()))
}
