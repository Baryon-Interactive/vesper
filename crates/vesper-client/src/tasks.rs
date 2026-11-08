use crate::{
    events::EventSink,
    install::{run_install, InstallRequest},
    state::{state_dir, task_path, AppState},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
};
use vesper_fetch::Materializer;
use vesper_format::Manifest;
use vesper_install::Installer;

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_task() -> u64 {
    NEXT_TASK.fetch_add(1, Ordering::Relaxed)
}

/// Install a manifest at a destination using the supplied materializer.
pub fn install_at<M: Materializer>(
    target: impl AsRef<Path>,
    manifest: &Manifest,
    materializer: &M,
) -> Result<()> {
    Installer::new(target.as_ref()).install(manifest, materializer)
}
/// Install a manifest using an existing installer and materializer.
pub fn install_with_materializer<M: Materializer>(
    installer: &Installer,
    manifest: &Manifest,
    materializer: &M,
) -> Result<()> {
    installer.install(manifest, materializer)
}

/// Persisted task status and transfer counters sent to event consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Unique task identifier.
    pub id: String,
    /// Published application identifier.
    pub app_id: String,
    /// Task or installation status string.
    pub status: String,
    /// Bytes transferred from remote sources.
    pub downloaded_bytes: u64,
    /// Bytes reused from local verified content.
    pub reused_bytes: u64,
    /// File bytes still awaiting materialization.
    pub remaining_bytes: u64,
    /// Per-source transfer rates in bytes per second.
    pub source_speeds: BTreeMap<String, u64>,
    /// User-facing operation or status message.
    pub message: Option<String>,
}

fn persist_task(task: &TaskRecord) {
    let _ = fs::create_dir_all(state_dir().join("tasks"));
    if let Ok(bytes) = serde_json::to_vec_pretty(task) {
        let _ = fs::write(task_path(&task.id), bytes);
    }
}

pub(crate) fn task_update(
    sink: &dyn EventSink,
    state: &AppState,
    id: &str,
    update: impl FnOnce(&mut TaskRecord),
) {
    if let Some(task) = state.tasks.lock().unwrap().get_mut(id) {
        update(task);
        persist_task(task);
        sink.emit("task-status", serde_json::to_value(task.clone()).unwrap());
    }
}

fn spawn_task(
    sink: Arc<dyn EventSink>,
    state: AppState,
    request: InstallRequest,
    task_id: String,
    cancel: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let result = run_install(
            sink.clone(),
            state.clone(),
            request,
            task_id.clone(),
            cancel.clone(),
        );
        if let Err(error) = result {
            if cancel.load(Ordering::Acquire) || error.to_string().contains("download paused") {
                task_update(sink.as_ref(), &state, &task_id, |task| {
                    task.status = "paused".into();
                    task.message = Some("Paused; verified chunks are retained for resume".into());
                });
            } else {
                task_update(sink.as_ref(), &state, &task_id, |task| {
                    task.status = "failed".into();
                    task.message = Some(error.to_string());
                });
            }
        }
    });
}

/// Queue an installation on a worker and report events through the sink.
pub fn start_install(
    request: InstallRequest,
    sink: Arc<dyn EventSink>,
    state: &AppState,
) -> Result<String> {
    let id = format!(
        "task-{}-{}",
        std::process::id(),
        NEXT_TASK.fetch_add(1, Ordering::Relaxed)
    );
    state.tasks.lock().unwrap().insert(
        id.clone(),
        TaskRecord {
            id: id.clone(),
            app_id: request.app_id.clone(),
            status: "queued".into(),
            downloaded_bytes: 0,
            reused_bytes: 0,
            remaining_bytes: 0,
            source_speeds: BTreeMap::new(),
            message: None,
        },
    );
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .requests
        .lock()
        .unwrap()
        .insert(id.clone(), request.clone());
    state
        .controls
        .lock()
        .unwrap()
        .insert(id.clone(), cancel.clone());
    spawn_task(sink.clone(), state.clone(), request, id.clone(), cancel);
    Ok(id)
}

/// Return the current in-memory task records.
pub fn list_tasks(state: &AppState) -> Vec<TaskRecord> {
    state.tasks.lock().unwrap().values().cloned().collect()
}

/// Request a pause between chunk requests and report the pausing status.
pub fn pause_task(task_id: &str, sink: &dyn EventSink, state: &AppState) -> Result<()> {
    let control = state
        .controls
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
        .context("task not found")?;
    control.store(true, Ordering::Release);
    task_update(sink, state, task_id, |task| task.status = "pausing".into());
    Ok(())
}

/// Resume a paused task using its retained verified chunks.
pub fn resume_task(task_id: &str, sink: Arc<dyn EventSink>, state: &AppState) -> Result<()> {
    let control = state
        .controls
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
        .context("task not found")?;
    let request = state
        .requests
        .lock()
        .unwrap()
        .get(task_id)
        .cloned()
        .context("task request is no longer available")?;
    control.store(false, Ordering::Release);
    spawn_task(
        sink.clone(),
        state.clone(),
        request,
        task_id.to_owned(),
        control,
    );
    Ok(())
}
