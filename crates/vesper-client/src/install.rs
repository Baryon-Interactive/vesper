use crate::{
    credentials,
    events::{EventSink, NoopEventSink},
    gateway, library,
    metadata::{self, fetch_manifest_bytes, ranked_source_bases},
    runtime::validate_runtime_rules,
    state::{installed_targets_path, task_path, AppState},
    tasks::{self, task_update, TaskRecord},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use vesper_fetch::{Materializer, MultiSourceFetcher};
use vesper_format::{CryptoContext, Manifest};
use vesper_install::Installer;
use vesper_meta::{AppTargetsSigned, Signed};
use zeroize::Zeroize;

/// Application, channel, index and destination for an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRequest {
    /// Published application identifier.
    pub app_id: String,
    /// Selected published release channel.
    pub channel: String,
    /// Base URL for verified signed metadata.
    pub metadata_base: String,
    /// Game installation directory.
    pub install_dir: String,
}

/// Installed and published versions with the update message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheck {
    /// Published application identifier.
    pub app_id: String,
    /// Currently installed display version.
    pub current_version: String,
    /// Published display version for the selected channel.
    pub target_version: String,
    /// Whether the published manifest differs from the installed manifest.
    pub available: bool,
    /// User-facing operation or status message.
    pub message: String,
}

struct TaskProgress {
    sink: Arc<dyn EventSink>,
    state: AppState,
    task_id: String,
    total: u64,
    processed: Mutex<u64>,
    unchanged: AtomicU64,
}

impl TaskProgress {
    fn transferred(&self, event: vesper_fetch::ProgressEvent) {
        task_update(self.sink.as_ref(), &self.state, &self.task_id, |task| {
            task.downloaded_bytes = event.downloaded_bytes;
            task.reused_bytes = event.reused_bytes + self.unchanged.load(Ordering::Acquire);
            task.source_speeds = event.source_speeds.into_iter().collect();
        });
    }

    fn advance(&self, bytes: u64) {
        let mut processed = self.processed.lock().unwrap();
        *processed = processed.saturating_add(bytes);
        let remaining = self.total.saturating_sub(*processed);
        task_update(self.sink.as_ref(), &self.state, &self.task_id, |task| {
            task.remaining_bytes = remaining;
            self.sink.emit(
                "download-progress",
                json!({
                    "task_id": self.task_id, "downloaded_bytes": task.downloaded_bytes,
                    "reused_bytes": task.reused_bytes, "remaining_bytes": remaining,
                    "source_speeds": task.source_speeds
                }),
            );
        });
    }
}

struct CancellableMaterializer<'a> {
    fetcher: &'a MultiSourceFetcher,
    cancel: &'a AtomicBool,
    progress: Arc<TaskProgress>,
}

impl Materializer for CancellableMaterializer<'_> {
    fn estimate_download_bytes(&self, manifest: &Manifest, file_indices: &[usize]) -> u64 {
        self.fetcher.estimate_download_bytes(manifest, file_indices)
    }

    fn cleanup_after_commit(&self) -> Result<()> {
        self.fetcher.cleanup_chunk_store()
    }

    fn materialize_file(&self, manifest: &Manifest, file_index: usize, out: &Path) -> Result<()> {
        let file = manifest
            .files
            .get(file_index)
            .context("file index out of range")?;
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(out)?;
        for chunk in &file.chunks {
            if self.cancel.load(Ordering::Acquire) {
                anyhow::bail!("download paused")
            }
            let bytes = self.fetcher.fetch_chunk(chunk.id, manifest)?;
            output.write_all(&bytes)?;
            self.progress.advance(bytes.len() as u64);
        }
        output.flush()?;
        output.sync_all()?;
        Ok(())
    }
}

pub(crate) fn run_install(
    sink: Arc<dyn EventSink>,
    state: AppState,
    request: InstallRequest,
    task_id: String,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    state
        .tasks
        .lock()
        .unwrap()
        .entry(task_id.clone())
        .or_insert_with(|| TaskRecord {
            id: task_id.clone(),
            app_id: request.app_id.clone(),
            status: "queued".into(),
            downloaded_bytes: 0,
            reused_bytes: 0,
            remaining_bytes: 0,
            source_speeds: BTreeMap::new(),
            message: None,
        });
    task_update(sink.as_ref(), &state, &task_id, |task| {
        task.status = "verifying".into()
    });
    let settings = state.settings.lock().unwrap().clone();
    let (targets, _) = metadata::verify_update(&request.metadata_base, &request.app_id)?;
    let channel = targets
        .signed
        .channels
        .get(&request.channel)
        .context("requested channel is not published")?;
    let crypto = if channel.encrypted {
        gateway::require_gateway(&settings.gateway_base)?;
        let token = credentials::load()?;
        let mut key_hex = gateway::depot_key(
            &settings.gateway_base,
            &request.app_id,
            channel.key_id,
            &token,
        )?;
        let result = CryptoContext::from_hex(&key_hex, channel.key_id, request.app_id.clone());
        key_hex.zeroize();
        Some(result?)
    } else {
        None
    };
    let bytes = fetch_manifest_bytes(
        &request.metadata_base,
        &request.app_id,
        &targets.signed,
        &request.channel,
        &settings.enabled_sources,
    )?;
    let manifest = if let Some(crypto) = &crypto {
        Manifest::decode_with_crypto(&bytes, crypto)?
    } else {
        Manifest::decode(&bytes)?
    };
    manifest.validate()?;
    if manifest.app_id != request.app_id || manifest.platform != "windows-x64" {
        anyhow::bail!("manifest identity does not match the requested application")
    }
    validate_runtime_rules(&manifest)?;
    let dir = PathBuf::from(&request.install_dir);
    fs::create_dir_all(&dir)?;
    let installer = Installer::new(&dir);
    let chunk_store = dir.join(".vesper").join("chunks");
    let mut fetcher = MultiSourceFetcher::new(ranked_source_bases(
        &request.metadata_base,
        &request.app_id,
        &targets.signed,
        &request.channel,
        &settings.enabled_sources,
    )?)?
    .with_chunk_store(chunk_store.clone())?
    .with_ranking_callback({
        let app_id = request.app_id.clone();
        move |ids| metadata::cache_source_ranking(&app_id, ids)
    });
    if let Some(crypto) = crypto.clone() {
        fetcher = fetcher.with_crypto(crypto);
    }
    let old_manifest_path = dir.join(".vesper").join("installed-manifest.cbor.zst");
    if old_manifest_path.is_file() {
        let rollback_meta = dir.join(".vesper").join("rollback-meta");
        fs::create_dir_all(&rollback_meta)?;
        fs::copy(
            &old_manifest_path,
            rollback_meta.join("installed-manifest.cbor.zst"),
        )?;
        if let Ok(current) = fs::read(dir.join(".vesper/current")) {
            fs::write(rollback_meta.join("current"), current)?;
        }
        if let Ok(targets) = fs::read(installed_targets_path(&dir)) {
            fs::write(rollback_meta.join("installed-targets.json"), targets)?;
        }
    }
    if let Ok(old_bytes) = fs::read(&old_manifest_path) {
        let old = if crypto.is_some() {
            crypto
                .as_ref()
                .and_then(|key| Manifest::decode_with_crypto(&old_bytes, key).ok())
        } else {
            Manifest::decode(&old_bytes).ok()
        };
        if let Some(old) = old {
            fetcher = fetcher.with_local_manifest(&old, &dir);
        }
    }
    let progress = Arc::new(TaskProgress {
        sink: sink.clone(),
        state: state.clone(),
        task_id: task_id.clone(),
        total: manifest.files.iter().map(|file| file.size).sum(),
        processed: Mutex::new(0),
        unchanged: AtomicU64::new(0),
    });
    let callback = progress.clone();
    fetcher = fetcher.with_progress_callback(move |event| callback.transferred(event));
    let materializer = CancellableMaterializer {
        fetcher: &fetcher,
        cancel: &cancel,
        progress: progress.clone(),
    };
    let estimate = installer.preflight(&manifest, &materializer)?;
    sink.emit("disk-preflight", json!({"task_id": task_id, "changed_bytes": estimate.changed_bytes, "download_bytes": estimate.download_bytes, "peak_bytes": estimate.peak_bytes}));
    let total: u64 = manifest.files.iter().map(|file| file.size).sum();
    let unchanged = total.saturating_sub(estimate.changed_bytes);
    progress.unchanged.store(unchanged, Ordering::Release);
    *progress.processed.lock().unwrap() = unchanged;
    task_update(sink.as_ref(), &state, &task_id, |task| {
        task.status = "installing".into();
        task.reused_bytes = unchanged;
        task.remaining_bytes = estimate.changed_bytes;
    });
    tasks::install_with_materializer(&installer, &manifest, &materializer).map_err(|error| {
        if cancel.load(Ordering::Acquire) {
            anyhow::anyhow!("download paused")
        } else {
            error
        }
    })?;
    fs::write(&old_manifest_path, &bytes)?;
    fs::write(
        dir.join(".vesper").join("metadata-version"),
        targets.signed.version.to_string(),
    )?;
    fs::write(
        installed_targets_path(&dir),
        serde_json::to_vec_pretty(&targets)?,
    )?;
    let _ = fs::remove_dir_all(&chunk_store);
    task_update(sink.as_ref(), &state, &task_id, |task| {
        task.status = "completed".into();
        task.remaining_bytes = 0;
        task.message = Some(format!(
            "{} {} installed",
            manifest.app_id, manifest.version
        ));
    });
    let _ = fs::remove_file(task_path(&task_id));
    Ok(())
}

fn installed_manifest(root: &Path, gateway_base: &str) -> Result<Manifest> {
    let bytes = fs::read(root.join(".vesper/installed-manifest.cbor.zst"))?;
    if bytes.starts_with(vesper_format::MANIFEST_MAGIC) {
        let (app_id, _, key_id) = Manifest::encrypted_header(&bytes)?;
        let token = credentials::load()?;
        let mut key_hex = gateway::depot_key(gateway_base, &app_id, key_id, &token)?;
        let crypto = CryptoContext::from_hex(&key_hex, key_id, app_id)?;
        key_hex.zeroize();
        Ok(Manifest::decode_with_crypto(&bytes, &crypto)?)
    } else {
        Ok(Manifest::decode(&bytes)?)
    }
}

/// Run the same verified installation pipeline used by the UI, without window events.
pub fn install_from_index(request: InstallRequest, state: &AppState) -> Result<()> {
    run_install(
        Arc::new(NoopEventSink),
        state.clone(),
        request,
        format!("sync-{}-{}", std::process::id(), tasks::next_task()),
        Arc::new(AtomicBool::new(false)),
    )
}

/// Restore backed-up game files and their installed metadata.
pub fn rollback_game(install_dir: &str) -> Result<()> {
    let root = PathBuf::from(install_dir);
    if !library::has_recoverable_backup(&root.join(".vesper")) {
        anyhow::bail!("no rollback backup is available");
    }
    Installer::new(&root).rollback()?;
    let rollback_meta = root.join(".vesper/rollback-meta");
    if rollback_meta.is_dir() {
        if rollback_meta.join("installed-manifest.cbor.zst").is_file() {
            fs::copy(
                rollback_meta.join("installed-manifest.cbor.zst"),
                root.join(".vesper/installed-manifest.cbor.zst"),
            )?;
            if let Ok(bytes) = fs::read(root.join(".vesper/installed-manifest.cbor.zst")) {
                if let Ok(manifest) = Manifest::decode(&bytes) {
                    fs::write(
                        root.join(".vesper/current"),
                        format!(
                            "{}\n{}\n",
                            manifest.version,
                            hex::encode(vesper_format::hash_bytes(&bytes))
                        ),
                    )?;
                }
            }
        }
        if rollback_meta.join("current").is_file() {
            fs::copy(rollback_meta.join("current"), root.join(".vesper/current"))?;
        }
        if rollback_meta.join("installed-targets.json").is_file() {
            fs::copy(
                rollback_meta.join("installed-targets.json"),
                installed_targets_path(&root),
            )?;
        }
        fs::remove_dir_all(rollback_meta)?;
    }
    Ok(())
}

/// Compare the installed manifest with the verified published channel.
pub fn check_update(install_dir: &str, state: &AppState) -> Result<UpdateCheck> {
    let settings = state.settings.lock().unwrap().clone();
    let root = PathBuf::from(install_dir);
    let app_id = root
        .file_name()
        .and_then(|value| value.to_str())
        .context("install directory has no application id")?
        .to_owned();
    let current_version = fs::read_to_string(root.join(".vesper/current"))
        .ok()
        .and_then(|value| value.lines().next().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    let channel = fs::read_to_string(root.join(".vesper/channel"))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "stable".into());
    let (targets, _) = metadata::verify_update(&settings.api_base, &app_id)?;
    let target = targets
        .signed
        .channels
        .get(&channel)
        .context("requested channel is not published")?;
    let installed_hash = hex::encode(vesper_format::hash_bytes(&fs::read(
        root.join(".vesper/installed-manifest.cbor.zst"),
    )?));
    let available = target.manifest.blake3 != installed_hash;
    let bytes = fetch_manifest_bytes(
        &settings.api_base,
        &app_id,
        &targets.signed,
        &channel,
        &settings.enabled_sources,
    )?;
    let game_version = if bytes.starts_with(vesper_format::MANIFEST_MAGIC) {
        Manifest::encrypted_header(&bytes)?.1
    } else {
        Manifest::decode(&bytes)?.version
    };
    Ok(UpdateCheck {
        app_id,
        current_version: current_version.clone(),
        target_version: game_version.clone(),
        available,
        message: if available {
            format!("Update available: v{game_version}")
        } else {
            "Up to date".into()
        },
    })
}

/// Return damaged installed files after manifest verification.
pub fn verify_game(install_dir: &str, state: &AppState) -> Result<Vec<String>> {
    let settings = state.settings.lock().unwrap().clone();
    let root = PathBuf::from(install_dir);
    let manifest = installed_manifest(&root, &settings.gateway_base)?;
    Installer::new(root).verify(&manifest)
}

/// Repair damaged installed files and report completion through the sink.
pub fn repair_game(install_dir: &str, state: &AppState, sink: &dyn EventSink) -> Result<String> {
    repair_from_index(install_dir, state, sink)
}

/// Repair through the verified pipeline and report completion to the event sink.
pub fn repair_from_index(
    install_dir: &str,
    state: &AppState,
    sink: &dyn EventSink,
) -> Result<String> {
    let settings = state.settings.lock().unwrap().clone();
    let root = PathBuf::from(install_dir);
    let app_id = root
        .file_name()
        .and_then(|value| value.to_str())
        .context("install directory has no application id")?;
    let channel = fs::read_to_string(root.join(".vesper/channel"))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "stable".into());
    let full_manifest = installed_manifest(&root, &settings.gateway_base)?;
    let damaged = Installer::new(&root).verify(&full_manifest)?;
    if damaged.is_empty() {
        return Ok("No files need repair".into());
    }
    let installed_hash = hex::encode(vesper_format::hash_bytes(&fs::read(
        root.join(".vesper/installed-manifest.cbor.zst"),
    )?));

    let targets: Signed<AppTargetsSigned> = if installed_targets_path(&root).is_file() {
        serde_json::from_slice(&fs::read(installed_targets_path(&root))?)?
    } else {
        let (latest, _) = metadata::verify_app(&settings.api_base, app_id)?;
        let matching = latest
            .signed
            .channels
            .get(&channel)
            .map(|entry| entry.manifest.blake3 == installed_hash)
            .unwrap_or(false);
        if !matching {
            anyhow::bail!(
                "installed version {} is no longer available for repair; update metadata first",
                full_manifest.version
            );
        }
        latest
    };
    let channel_meta = targets
        .signed
        .channels
        .get(&channel)
        .context("installed channel metadata is unavailable; update metadata first")?;
    if channel_meta.manifest.blake3 != installed_hash {
        anyhow::bail!(
            "installed version {} is no longer available for repair; update metadata first",
            full_manifest.version
        );
    }
    let repair_files = full_manifest
        .files
        .iter()
        .filter(|file| damaged.iter().any(|path| path == &file.path))
        .cloned()
        .collect::<Vec<_>>();
    let mut repair_manifest = full_manifest.clone();
    repair_manifest.files = repair_files;
    let mut fetcher = MultiSourceFetcher::new(ranked_source_bases(
        &settings.api_base,
        app_id,
        &targets.signed,
        &channel,
        &settings.enabled_sources,
    )?)?
    .with_chunk_store(root.join(".vesper/repair-chunks"))?
    .with_local_manifest(&full_manifest, &root)
    .with_ranking_callback({
        let app_id = app_id.to_owned();
        move |ids| metadata::cache_source_ranking(&app_id, ids)
    });
    if full_manifest.encrypted {
        let key_id = full_manifest.key_id;
        let token = credentials::load()?;
        let mut key_hex = gateway::depot_key(&settings.gateway_base, app_id, key_id, &token)?;
        let crypto = CryptoContext::from_hex(&key_hex, key_id, app_id.to_owned())?;
        key_hex.zeroize();
        fetcher = fetcher.with_crypto(crypto);
    }
    let repair_root = root
        .join(".vesper")
        .join(format!("repair-{}", std::process::id()));
    let _ = fs::remove_dir_all(&repair_root);
    fs::create_dir_all(&repair_root)?;
    let result = (|| -> Result<()> {
        Installer::new(&repair_root).install(&repair_manifest, &fetcher)?;
        for file in &repair_manifest.files {
            let source = repair_root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            let target = root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, target)?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&repair_root);
    result?;
    sink.emit(
        "repair-completed",
        json!({"app_id": app_id, "files": damaged}),
    );
    Ok(format!("Repaired {} file(s)", damaged.len()))
}

/// Remove installed files while honoring the user-file deletion preference.
pub fn uninstall_game(install_dir: &str, delete_user_files: bool, state: &AppState) -> Result<()> {
    let settings = state.settings.lock().unwrap().clone();
    let root = PathBuf::from(install_dir);
    let manifest = installed_manifest(&root, &settings.gateway_base)?;
    Installer::new(&root).uninstall(&manifest, delete_user_files)?;
    if root.is_dir() && fs::read_dir(&root)?.next().is_none() {
        fs::remove_dir(&root)?;
    }
    Ok(())
}

/// Persist the selected channel in the control directory.
pub fn set_game_channel(install_dir: &str, channel: &str) -> Result<()> {
    let root = PathBuf::from(install_dir);
    fs::create_dir_all(root.join(".vesper"))?;
    fs::write(root.join(".vesper/channel"), channel)?;
    Ok(())
}
