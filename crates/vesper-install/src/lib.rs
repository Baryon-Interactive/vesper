//! File-granular install transactions with crash recovery.

use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use vesper_fetch::Materializer;
use vesper_format::{hash_file, Manifest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub target: String,
    pub staged: Option<String>,
    pub backup: Option<String>,
    pub delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub txn_id: String,
    pub version: String,
    pub manifest_sha256: String,
    #[serde(default)]
    pub manifest_path: Option<String>,
    pub commit: bool,
    pub entries: Vec<JournalEntry>,
}

pub struct Installer {
    root: PathBuf,
    control: PathBuf,
    hooks: InstallHooks,
}

#[derive(Debug, Clone, Default)]
pub struct InstallHooks {
    /// Test-only fault injection point immediately before COMMITTED is written.
    pub fail_before_commit: bool,
    /// Test-only fault injection point immediately after COMMITTED is written.
    pub fail_after_commit: bool,
    /// Test-only staging write failure, modelling ENOSPC/ERROR_DISK_FULL.
    pub fail_staging_writes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedProcess {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskEstimate {
    pub changed_bytes: u64,
    pub download_bytes: u64,
    pub peak_bytes: u64,
}

impl Installer {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let control = root.join(".vesper");
        Self {
            root,
            control,
            hooks: InstallHooks::default(),
        }
    }

    pub fn with_hooks(mut self, hooks: InstallHooks) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn recover(&self) -> Result<()> {
        fs::create_dir_all(&self.control)?;
        let journal_path = self.control.join("journal.json");
        if !journal_path.exists() {
            return Ok(());
        }
        let journal: Journal = serde_json::from_slice(&fs::read(&journal_path)?)?;
        if !journal.commit {
            let staging_root = self.control.join("staging").join(&journal.txn_id);
            let _ = fs::remove_dir_all(staging_root);
            fs::remove_file(journal_path)?;
            return Ok(());
        }
        self.apply_journal(&journal)?;
        self.finalize_manifest(&journal)?;
        fs::remove_file(journal_path)?;
        let _ = fs::remove_dir_all(self.control.join("staging").join(&journal.txn_id));
        clear_chunk_store(&self.control)?;
        Ok(())
    }

    pub fn estimate_peak(&self, manifest: &Manifest) -> Result<DiskEstimate> {
        manifest.validate()?;
        let previous = self.read_installed_manifest().ok();
        let changed_bytes = self
            .changed_file_indices(manifest, previous.as_ref())?
            .iter()
            .filter_map(|index| manifest.files.get(*index))
            .map(|file| file.size)
            .sum();
        Ok(DiskEstimate {
            changed_bytes,
            download_bytes: 0,
            peak_bytes: changed_bytes.saturating_add(512 * 1024 * 1024),
        })
    }

    pub fn preflight<F: Materializer>(
        &self,
        manifest: &Manifest,
        fetcher: &F,
    ) -> Result<DiskEstimate> {
        manifest.validate()?;
        let previous = self.read_installed_manifest().ok();
        let indices = self.changed_file_indices(manifest, previous.as_ref())?;
        let changed_bytes = indices
            .iter()
            .filter_map(|index| manifest.files.get(*index))
            .map(|file| file.size)
            .sum();
        let download_bytes = fetcher.estimate_download_bytes(manifest, &indices);
        Ok(DiskEstimate {
            changed_bytes,
            download_bytes,
            peak_bytes: changed_bytes
                .saturating_add(download_bytes)
                .saturating_add(512 * 1024 * 1024),
        })
    }

    fn changed_file_indices(
        &self,
        manifest: &Manifest,
        previous: Option<&Manifest>,
    ) -> Result<Vec<usize>> {
        let previous_files = previous
            .map(|m| {
                m.files
                    .iter()
                    .map(|f| (f.path.as_str(), f))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut indices = Vec::new();
        for (index, file) in manifest.files.iter().enumerate() {
            let target = self.root.join(path_to_os(&file.path));
            let unchanged = previous_files
                .get(file.path.as_str())
                .map(|old| {
                    old.content_hash == file.content_hash
                        && target.is_file()
                        && target
                            .metadata()
                            .map(|metadata| metadata.len() == file.size)
                            .unwrap_or(false)
                })
                .unwrap_or(false);
            if !unchanged {
                indices.push(index);
            }
        }
        Ok(indices)
    }

    pub fn install<F: Materializer>(&self, manifest: &Manifest, fetcher: &F) -> Result<()> {
        manifest.validate()?;
        self.recover()?;
        fs::create_dir_all(&self.control)?;
        let previous = self.read_installed_manifest().ok();
        let estimate = self.preflight(manifest, fetcher)?;
        if let Some(available) = available_space(&self.root) {
            if available < estimate.peak_bytes {
                anyhow::bail!(
                    "insufficient disk space: need {}, available {}",
                    estimate.peak_bytes,
                    available
                );
            }
        }
        let previous_files: BTreeMap<String, &vesper_format::FileEntry> = previous
            .as_ref()
            .map(|m| m.files.iter().map(|f| (f.path.clone(), f)).collect())
            .unwrap_or_default();
        let current_paths: BTreeSet<String> =
            manifest.files.iter().map(|f| f.path.clone()).collect();
        let txn_id = format!("txn-{}-{}", std::process::id(), unix_seconds());
        let staging_root = self.control.join("staging").join(&txn_id);
        let backup_root = self.control.join("backup").join(&txn_id);
        fs::create_dir_all(&staging_root)?;
        fs::create_dir_all(&backup_root)?;
        if let Some(previous) = &previous {
            fs::write(
                backup_root.join(".previous-manifest.cbor.zst"),
                previous.encode()?,
            )?;
        }

        let mut entries = Vec::new();
        let mut work_items = Vec::new();
        for (index, file) in manifest.files.iter().enumerate() {
            let target = self.root.join(path_to_os(&file.path));
            let user_file = manifest
                .install
                .user_files
                .iter()
                .any(|pattern| matches_user_path(pattern, &file.path));
            if user_file && target.exists() {
                continue;
            }
            let unchanged = previous_files
                .get(&file.path)
                .map(|old| {
                    old.content_hash == file.content_hash
                        && target.is_file()
                        && target
                            .metadata()
                            .map(|metadata| metadata.len() == file.size)
                            .unwrap_or(false)
                })
                .unwrap_or(false);
            if unchanged {
                continue;
            }
            let staged = staging_root.join(path_to_os(&file.path));
            work_items.push((index, staged, target));
        }
        if self.hooks.fail_staging_writes {
            let _ = fs::remove_dir_all(&staging_root);
            anyhow::bail!("disk full while writing staging");
        }
        let work_indices = work_items
            .iter()
            .map(|(index, _, _)| *index)
            .collect::<Vec<_>>();
        if let Err(error) = fetcher.materialize_files(manifest, &work_indices, &staging_root) {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
        for (index, staged, target) in work_items {
            let file = manifest
                .files
                .get(index)
                .context("file index out of range during staging")?;
            if hash_file(&staged)? != file.content_hash {
                let _ = fs::remove_dir_all(&staging_root);
                anyhow::bail!("materialized file hash mismatch for {}", file.path);
            }
            entries.push(JournalEntry {
                target: file.path.clone(),
                staged: Some(path_from_root(&staged, &self.root)),
                backup: if target.exists() {
                    Some(path_from_root(
                        &backup_root.join(path_to_os(&file.path)),
                        &self.root,
                    ))
                } else {
                    None
                },
                delete: false,
            });
        }
        for old_path in previous_files.keys() {
            if !current_paths.contains(old_path)
                && !manifest
                    .install
                    .user_files
                    .iter()
                    .any(|pattern| matches_user_path(pattern, old_path))
            {
                let target = self.root.join(path_to_os(old_path));
                if target.exists() {
                    entries.push(JournalEntry {
                        target: old_path.clone(),
                        staged: None,
                        backup: Some(path_from_root(
                            &backup_root.join(path_to_os(old_path)),
                            &self.root,
                        )),
                        delete: true,
                    });
                }
            }
        }

        let manifest_bytes = manifest.encode()?;
        let staged_manifest = staging_root.join("manifest.cbor.zst");
        fs::write(&staged_manifest, &manifest_bytes)?;
        let journal = Journal {
            txn_id: txn_id.clone(),
            version: manifest.version.clone(),
            manifest_sha256: hex::encode(vesper_format::hash_bytes(&manifest_bytes)),
            manifest_path: Some(path_from_root(&staged_manifest, &self.root)),
            commit: false,
            entries,
        };
        self.write_journal(&journal)
            .context("write install journal")?;
        if self.hooks.fail_before_commit {
            anyhow::bail!("injected failure before COMMITTED");
        }
        let mut committed = journal.clone();
        committed.commit = true;
        self.write_journal(&committed)
            .context("commit install journal")?;
        if self.hooks.fail_after_commit {
            anyhow::bail!("injected failure after COMMITTED");
        }
        if let Err(error) = self.apply_journal(&committed) {
            let _ = self.recover();
            return Err(error);
        }
        self.finalize_manifest(&committed)
            .context("finalize installed manifest")?;
        fs::remove_file(self.control.join("journal.json"))?;
        let _ = fs::remove_dir_all(&staging_root);
        fetcher
            .cleanup_after_commit()
            .context("cleanup verified download chunks")?;
        Ok(())
    }

    pub fn verify(&self, manifest: &Manifest) -> Result<Vec<String>> {
        let mut invalid = Vec::new();
        let expected = manifest
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        for file in &manifest.files {
            if manifest
                .install
                .user_files
                .iter()
                .any(|pattern| matches_user_path(pattern, &file.path))
            {
                continue;
            }
            let path = self.root.join(path_to_os(&file.path));
            if !path.is_file() || hash_file(&path)? != file.content_hash {
                invalid.push(file.path.clone());
            }
        }
        if self.root.is_dir() {
            for entry in walkdir::WalkDir::new(&self.root).follow_links(false) {
                let entry = entry?;
                if entry.path() == self.control || entry.path().starts_with(&self.control) {
                    continue;
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&self.root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
                if !expected.contains(relative.as_str())
                    && !manifest
                        .install
                        .user_files
                        .iter()
                        .any(|pattern| matches_user_path(pattern, &relative))
                {
                    invalid.push(format!("extra: {relative}"));
                }
            }
        }
        Ok(invalid)
    }

    pub fn uninstall(&self, manifest: &Manifest, delete_user_files: bool) -> Result<()> {
        self.recover()?;
        for file in &manifest.files {
            if !delete_user_files
                && manifest
                    .install
                    .user_files
                    .iter()
                    .any(|pattern| matches_user_path(pattern, &file.path))
            {
                continue;
            }
            let path = self.root.join(path_to_os(&file.path));
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        let _ = fs::remove_dir_all(&self.control);
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        let backup_root = self.control.join("backup");
        if !backup_root.is_dir() {
            anyhow::bail!("no rollback backup is available");
        }
        let transactions = fs::read_dir(&backup_root)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect::<Vec<_>>();
        let Some(transaction) = transactions.into_iter().max_by_key(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        }) else {
            anyhow::bail!("no rollback backup is available");
        };
        let previous_manifest_path = transaction.path().join(".previous-manifest.cbor.zst");
        let previous = if previous_manifest_path.is_file() {
            Some(Manifest::decode(&fs::read(&previous_manifest_path)?)?)
        } else {
            None
        };
        let current = self.read_installed_manifest().ok();
        if let (Some(previous), Some(current)) = (&previous, &current) {
            let previous_paths = current_paths(previous);
            for file in &current.files {
                if !previous_paths.contains(file.path.as_str())
                    && !current
                        .install
                        .user_files
                        .iter()
                        .any(|pattern| matches_user_path(pattern, &file.path))
                {
                    let target = self.root.join(path_to_os(&file.path));
                    if target.is_file() {
                        remove_with_retry(&target)?;
                    }
                }
            }
        }
        for entry in walk_files(transaction.path())?.into_iter().filter(|path| {
            path.file_name().and_then(|name| name.to_str()) != Some(".previous-manifest.cbor.zst")
        }) {
            let relative = entry.strip_prefix(transaction.path()).unwrap();
            let target = self.root.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            if target.exists() {
                remove_with_retry(&target)?;
            }
            rename_with_retry(&entry, &target)?;
        }
        fs::remove_dir_all(transaction.path())?;
        if let Some(previous) = previous {
            let bytes = previous.encode()?;
            fs::write(self.control.join("installed-manifest.cbor.zst"), &bytes)?;
            fs::write(
                self.control.join("current"),
                format!(
                    "{}\n{}\n",
                    previous.version,
                    hex::encode(vesper_format::hash_bytes(&bytes))
                ),
            )?;
        }
        remove_empty_dirs(&self.root, &self.control)?;
        Ok(())
    }

    pub fn cleanup_backup(&self) -> Result<()> {
        if self.control.join("backup").is_dir() {
            fs::remove_dir_all(self.control.join("backup"))?;
        }
        Ok(())
    }

    fn apply_journal(&self, journal: &Journal) -> Result<()> {
        for entry in &journal.entries {
            let target = self.root.join(path_to_os(&entry.target));
            let locked = restart_manager_processes(std::slice::from_ref(&target))?;
            if !locked.is_empty() {
                let details = locked
                    .iter()
                    .map(|p| format!("{}:{}", p.pid, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!("target is in use by process(es): {details}");
            }
            let backup = entry.backup.as_ref().map(|p| self.root.join(path_to_os(p)));
            if let Some(backup) = backup {
                if target.exists() && !backup.exists() {
                    if let Some(parent) = backup.parent() {
                        ensure_safe_parent(parent)?;
                    }
                    rename_with_retry(&target, &backup)
                        .with_context(|| format!("backup {}", target.display()))?;
                }
            }
            if let Some(staged) = &entry.staged {
                let staged = self.root.join(path_to_os(staged));
                if staged.exists() {
                    if let Some(parent) = target.parent() {
                        ensure_safe_parent(parent)?;
                    }
                    rename_with_retry(&staged, &target)
                        .with_context(|| format!("install {}", target.display()))?;
                }
            }
        }
        Ok(())
    }

    fn write_journal(&self, journal: &Journal) -> Result<()> {
        let path = self.control.join("journal.json");
        let tmp = self.control.join("journal.json.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.sync_all()?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    fn read_installed_manifest(&self) -> Result<Manifest> {
        Manifest::decode(&fs::read(self.control.join("installed-manifest.cbor.zst"))?)
    }

    fn finalize_manifest(&self, journal: &Journal) -> Result<()> {
        let Some(staged_path) = &journal.manifest_path else {
            return Ok(());
        };
        let staged = self.root.join(path_to_os(staged_path));
        if !staged.exists() {
            anyhow::bail!("journal manifest is missing: {}", staged.display());
        }
        let manifest_tmp = self.control.join("installed-manifest.cbor.zst.tmp");
        fs::copy(&staged, &manifest_tmp)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&manifest_tmp)
            .context("open staged installed manifest")?
            .sync_all()
            .context("sync staged installed manifest")?;
        replace_file(
            manifest_tmp,
            self.control.join("installed-manifest.cbor.zst"),
        )
        .context("replace installed manifest")?;
        let current_tmp = self.control.join("current.tmp");
        fs::write(
            &current_tmp,
            format!("{}\n{}\n", journal.version, journal.manifest_sha256),
        )?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&current_tmp)
            .context("open current marker")?
            .sync_all()
            .context("sync current marker")?;
        replace_file(current_tmp, self.control.join("current"))
            .context("replace current marker")?;
        Ok(())
    }
}

fn replace_file(from: PathBuf, to: PathBuf) -> Result<()> {
    if to.exists() {
        fs::remove_file(&to)?;
    }
    fs::rename(from, to)?;
    Ok(())
}

fn clear_chunk_store(control: &Path) -> Result<()> {
    let store = control.join("chunks");
    if !store.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&store)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() {
            fs::remove_file(path)?;
        } else if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn ensure_safe_parent(parent: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        if current.exists() && fs::symlink_metadata(&current)?.file_type().is_symlink() {
            anyhow::bail!(
                "reparse/symlink parent is not allowed: {}",
                current.display()
            );
        }
    }
    fs::create_dir_all(parent)?;
    Ok(())
}

fn rename_with_retry(from: &Path, to: &Path) -> Result<()> {
    let mut last = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for attempt in 0..15 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !retryable_io(&error) || std::time::Instant::now() >= deadline {
                    return Err(error.into());
                }
                last = Some(error);
                let jitter = rand::thread_rng().gen_range(0..=25u64);
                let delay = 10u64.saturating_mul(1u64 << attempt.min(8)).min(500) + jitter;
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
    Err(last.unwrap().into())
}

fn remove_with_retry(path: &Path) -> Result<()> {
    let mut last = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for attempt in 0..15 {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !retryable_io(&error) || std::time::Instant::now() >= deadline {
                    return Err(error.into());
                }
                last = Some(error);
                let jitter = rand::thread_rng().gen_range(0..=25u64);
                let delay = 10u64.saturating_mul(1u64 << attempt.min(8)).min(500) + jitter;
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
    }
    Err(last.unwrap().into())
}

fn retryable_io(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(5 | 32)) {
        return true;
    }
    false
}

/// Return processes that currently hold a file registered with Windows Restart
/// Manager. On non-Windows platforms the operation is intentionally a no-op.
pub fn restart_manager_processes(paths: &[PathBuf]) -> Result<Vec<LockedProcess>> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::RestartManager::*;
        let mut session = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        let rc = unsafe { RmStartSession(&mut session, 0, key.as_mut_ptr()) };
        if rc != 0 {
            anyhow::bail!("RmStartSession failed: {rc}");
        }
        let result = (|| {
            let wide = paths
                .iter()
                .map(|p| {
                    p.as_os_str()
                        .encode_wide()
                        .chain(std::iter::once(0))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let ptrs = wide.iter().map(|p| p.as_ptr()).collect::<Vec<_>>();
            let rc = unsafe {
                RmRegisterResources(
                    session,
                    ptrs.len() as u32,
                    ptrs.as_ptr(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc != 0 {
                anyhow::bail!("RmRegisterResources failed: {rc}");
            }
            let mut needed = 0u32;
            let mut count = 0u32;
            let mut reasons = 0u32;
            let first = unsafe {
                RmGetList(
                    session,
                    &mut needed,
                    &mut count,
                    std::ptr::null_mut(),
                    &mut reasons,
                )
            };
            if first != 234 && first != 0 {
                anyhow::bail!("RmGetList failed: {first}");
            }
            if needed == 0 {
                return Ok(Vec::new());
            }
            let mut entries =
                vec![unsafe { std::mem::zeroed::<RM_PROCESS_INFO>() }; needed as usize];
            count = needed;
            let rc = unsafe {
                RmGetList(
                    session,
                    &mut needed,
                    &mut count,
                    entries.as_mut_ptr(),
                    &mut reasons,
                )
            };
            if rc != 0 {
                anyhow::bail!("RmGetList failed: {rc}");
            }
            Ok(entries
                .into_iter()
                .take(count as usize)
                .map(|entry| {
                    let end = entry
                        .strAppName
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.strAppName.len());
                    LockedProcess {
                        pid: entry.Process.dwProcessId,
                        name: String::from_utf16_lossy(&entry.strAppName[..end]),
                    }
                })
                .collect())
        })();
        unsafe {
            RmEndSession(session);
        }
        result
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Ok(Vec::new())
    }
}

fn walk_files(root: PathBuf) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            anyhow::bail!("symlink in backup");
        }
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

fn available_space(path: &Path) -> Option<u64> {
    #[cfg(not(windows))]
    let _ = path;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut free = 0u64;
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut free,
            )
        };
        if ok != 0 {
            return Some(free);
        }
    }
    None
}

fn path_to_os(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn path_from_root(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn current_paths(manifest: &Manifest) -> BTreeSet<&str> {
    manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

fn remove_empty_dirs(root: &Path, control: &Path) -> Result<()> {
    let mut directories = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_dir())
        .map(|entry| entry.into_path())
        .filter(|path| path != control && !path.starts_with(control))
        .collect::<Vec<_>>();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        if fs::read_dir(&directory)?.next().is_none() {
            let _ = fs::remove_dir(&directory);
        }
    }
    Ok(())
}

fn matches_user_path(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(suffix) = pattern.strip_prefix("*") {
        return path.ends_with(suffix);
    }
    pattern == path
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use vesper_format::{build_from_directory, ChunkingParams};

    struct RecordingMaterializer {
        inner: vesper_fetch::Fetcher,
        calls: Arc<Mutex<Vec<usize>>>,
    }

    impl Materializer for RecordingMaterializer {
        fn materialize_file(
            &self,
            manifest: &Manifest,
            file_index: usize,
            out: &Path,
        ) -> Result<()> {
            self.calls.lock().unwrap().push(file_index);
            self.inner.materialize_file(manifest, file_index, out)
        }

        fn estimate_download_bytes(&self, manifest: &Manifest, file_indices: &[usize]) -> u64 {
            self.inner.estimated_download_bytes(manifest, file_indices)
        }
    }

    #[test]
    fn update_skips_unchanged_files_and_rollback_restores_complete_version() {
        let v1 = tempfile::tempdir().unwrap();
        fs::write(v1.path().join("same.bin"), b"same").unwrap();
        fs::write(v1.path().join("change.bin"), b"old").unwrap();
        fs::write(v1.path().join("removed.bin"), b"remove").unwrap();
        let v2 = tempfile::tempdir().unwrap();
        fs::write(v2.path().join("same.bin"), b"same").unwrap();
        fs::write(v2.path().join("change.bin"), b"new").unwrap();
        fs::create_dir_all(v2.path().join("new-directory")).unwrap();
        fs::write(v2.path().join("new-directory/added.bin"), b"added").unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let b1 = tempfile::tempdir().unwrap();
        let b2 = tempfile::tempdir().unwrap();
        let m1 = build_from_directory(
            v1.path(),
            b1.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let m2 = build_from_directory(
            v2.path(),
            b2.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            params,
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let target = tempfile::tempdir().unwrap();
        let installer = Installer::new(target.path());
        installer
            .install(
                &m1,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(b1.path().to_path_buf()))
                    .unwrap(),
            )
            .unwrap();
        let same_meta = fs::metadata(target.path().join("same.bin")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorder = RecordingMaterializer {
            inner: vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(b2.path().to_path_buf()))
                .unwrap(),
            calls: calls.clone(),
        };
        installer.install(&m2, &recorder).unwrap();
        let unchanged_index = m2
            .files
            .iter()
            .position(|file| file.path == "same.bin")
            .unwrap();
        assert!(!calls.lock().unwrap().contains(&unchanged_index));
        let after_meta = fs::metadata(target.path().join("same.bin")).unwrap();
        assert_eq!(same_meta.len(), after_meta.len());
        assert_eq!(
            same_meta.modified().unwrap(),
            after_meta.modified().unwrap()
        );
        let backup_files = walk_files(target.path().join(".vesper/backup")).unwrap();
        assert!(backup_files.iter().any(|path| path.ends_with("change.bin")));
        assert!(backup_files
            .iter()
            .any(|path| path.ends_with("removed.bin")));
        assert!(!backup_files.iter().any(|path| path.ends_with("same.bin")));
        fs::write(target.path().join("unexpected.txt"), b"extra").unwrap();
        assert!(installer
            .verify(&m2)
            .unwrap()
            .iter()
            .any(|path| path == "extra: unexpected.txt"));
        fs::remove_file(target.path().join("unexpected.txt")).unwrap();
        installer.rollback().unwrap();
        assert!(installer.verify(&m1).unwrap().is_empty());
        assert!(!target.path().join("new-directory/added.bin").exists());
        assert!(!target.path().join("new-directory").exists());
    }

    #[test]
    fn installer_downloads_flat_template_and_refreshes_expired_redirect() {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("game.bin"), vec![b'x'; 100_000]).unwrap();
        let build = tempfile::tempdir().unwrap();
        let built = build_from_directory(
            input.path(),
            build.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64 * 1024,
                avg_size: 128 * 1024,
                max_size: 256 * 1024,
            },
            1024 * 1024,
        )
        .unwrap();
        let pack = built.manifest.packs[0].pack_id.clone();
        let mut manifest = built.manifest.clone();
        manifest.packs[0].release_tag = Some("v1".into());
        let bytes = Arc::new(fs::read(build.path().join("packs").join(&pack)).unwrap());
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let original_requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server_bytes = bytes.clone();
        let server_count = original_requests.clone();
        let server_stop = stop.clone();
        let thread = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let mut request = [0u8; 4096];
                let size = stream.read(&mut request).unwrap_or(0);
                let text = String::from_utf8_lossy(&request[..size]);
                let path = text
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("");
                if path.starts_with("/releases/download/v1/") {
                    server_count.fetch_add(1, Ordering::SeqCst);
                    let expiry = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 1;
                    let days = (expiry / 86_400) as i64;
                    let seconds = expiry % 86_400;
                    let z = days + 719_468;
                    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
                    let doe = z - era * 146_097;
                    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
                    let year = yoe + era * 400;
                    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
                    let mp = (5 * doy + 2) / 153;
                    let day = doy - (153 * mp + 2) / 5 + 1;
                    let month = mp + if mp < 10 { 3 } else { -9 };
                    let year = year + if month <= 2 { 1 } else { 0 };
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: /signed/{}?se={:04}-{:02}-{:02}T{:02}%3A{:02}%3A{:02}Z\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        pack,
                        year,
                        month,
                        day,
                        seconds / 3600,
                        (seconds / 60) % 60,
                        seconds % 60
                    );
                    let _ = stream.write_all(response.as_bytes());
                    continue;
                }
                if !path.starts_with("/signed/") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                }
                let start = text
                    .lines()
                    .find_map(|line| line.strip_prefix("Range: bytes="))
                    .and_then(|value| value.split('-').next())
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let end = text
                    .lines()
                    .find_map(|line| line.strip_prefix("Range: bytes="))
                    .and_then(|value| value.split('-').nth(1))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(server_bytes.len() - 1)
                    .min(server_bytes.len() - 1);
                let body = &server_bytes[start..=end];
                let header = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    start,
                    end,
                    server_bytes.len(),
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        thread::sleep(Duration::from_millis(20));

        let target = tempfile::tempdir().unwrap();
        let installer = Installer::new(target.path());
        let source = format!("{base}/releases/download/{{release}}/{{pack_id}}");
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::with_default_release(
                vesper_fetch::Source::Http(source.clone()),
                "v2",
            ),
        ])
        .unwrap();
        installer.install(&manifest, &fetcher).unwrap();
        assert_eq!(
            fs::read(target.path().join("game.bin")).unwrap(),
            vec![b'x'; 100_000]
        );
        thread::sleep(Duration::from_secs(2));
        fs::write(target.path().join("game.bin"), b"corrupt").unwrap();
        installer.install(&manifest, &fetcher).unwrap();
        assert_eq!(
            fs::read(target.path().join("game.bin")).unwrap(),
            vec![b'x'; 100_000]
        );
        assert!(original_requests.load(Ordering::SeqCst) >= 2);
        stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(base.trim_start_matches("http://"));
        let _ = thread.join();
    }

    #[test]
    fn update_preserves_user_files_and_replaces_content() {
        let input_one = tempfile::tempdir().unwrap();
        fs::create_dir_all(input_one.path().join("Saved")).unwrap();
        fs::write(input_one.path().join("game.exe"), b"v1").unwrap();
        fs::write(input_one.path().join("Saved/config.ini"), b"default-v1").unwrap();
        let input_two = tempfile::tempdir().unwrap();
        fs::create_dir_all(input_two.path().join("Saved")).unwrap();
        fs::write(input_two.path().join("game.exe"), b"v2").unwrap();
        fs::write(input_two.path().join("Saved/config.ini"), b"default-v2").unwrap();
        let build_one = tempfile::tempdir().unwrap();
        let build_two = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let mut manifest_one = build_from_directory(
            input_one.path(),
            build_one.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let mut manifest_two = build_from_directory(
            input_two.path(),
            build_two.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            params,
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        manifest_one.install.user_files = vec!["Saved/**".into()];
        manifest_two.install.user_files = vec!["Saved/**".into()];
        fs::write(
            build_one.path().join("manifest.cbor.zst"),
            manifest_one.encode().unwrap(),
        )
        .unwrap();
        fs::write(
            build_two.path().join("manifest.cbor.zst"),
            manifest_two.encode().unwrap(),
        )
        .unwrap();
        let target = tempfile::tempdir().unwrap();
        let installer = Installer::new(target.path());
        installer
            .install(
                &manifest_one,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    build_one.path().to_path_buf(),
                ))
                .unwrap(),
            )
            .unwrap();
        fs::write(target.path().join("Saved/config.ini"), b"player data").unwrap();
        installer
            .install(
                &manifest_two,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    build_two.path().to_path_buf(),
                ))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(fs::read(target.path().join("game.exe")).unwrap(), b"v2");
        assert_eq!(
            fs::read(target.path().join("Saved/config.ini")).unwrap(),
            b"player data"
        );
        assert!(installer.verify(&manifest_two).unwrap().is_empty());
    }

    #[test]
    fn committed_install_clears_verified_chunk_store() {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("game.bin"), vec![b'x'; 100_000]).unwrap();
        let build = tempfile::tempdir().unwrap();
        let manifest = build_from_directory(
            input.path(),
            build.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64 * 1024,
                avg_size: 128 * 1024,
                max_size: 256 * 1024,
            },
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let target = tempfile::tempdir().unwrap();
        let store = target.path().join(".vesper/chunks");
        let fetcher =
            vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(build.path().to_path_buf()))
                .unwrap()
                .with_chunk_store(&store)
                .unwrap();
        Installer::new(target.path())
            .install(&manifest, &fetcher)
            .unwrap();
        assert!(store.is_dir());
        assert_eq!(fs::read_dir(store).unwrap().count(), 0);
        assert!(Installer::new(target.path())
            .verify(&manifest)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn committed_before_or_after_recovery_is_deterministic() {
        let target = tempfile::tempdir().unwrap();
        let installer = Installer::new(target.path());
        fs::create_dir_all(target.path().join(".vesper/staging/txn")).unwrap();
        let journal = Journal {
            txn_id: "txn".into(),
            version: "1".into(),
            manifest_sha256: "00".into(),
            manifest_path: None,
            commit: false,
            entries: Vec::new(),
        };
        installer.write_journal(&journal).unwrap();
        installer.recover().unwrap();
        assert!(!target.path().join(".vesper/journal.json").exists());
    }

    #[test]
    fn disk_peak_preflight_adds_fixed_headroom() {
        let target = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            schema: "Vesper/1".into(),
            app_id: "demo".into(),
            platform: "windows-x64".into(),
            version: "1".into(),
            created_at: "unix:0".into(),
            key_id: 0,
            encrypted: false,
            chunking: ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
            files: Vec::new(),
            chunks: BTreeMap::new(),
            packs: Vec::new(),
            install: vesper_format::InstallRules {
                user_files: Vec::new(),
                runtimes: Vec::new(),
                launch: None,
            },
        };
        let estimate = Installer::new(target.path())
            .estimate_peak(&manifest)
            .unwrap();
        assert_eq!(estimate.peak_bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn rollback_without_backup_reports_failure() {
        let target = tempfile::tempdir().unwrap();
        assert!(Installer::new(target.path()).rollback().is_err());
    }

    #[test]
    fn retry_after_non_lock_error_is_bounded_and_calls_retry_path() {
        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("held.exe");
        let started = std::time::Instant::now();
        let error = rename_with_retry(&path, &target.path().join("new.exe")).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound)
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }

    #[cfg(windows)]
    #[test]
    fn retry_after_exclusive_file_open_releases_and_succeeds() {
        use std::os::windows::fs::OpenOptionsExt;
        let target = tempfile::tempdir().unwrap();
        let from = target.path().join("held.exe");
        let to = target.path().join("moved.exe");
        fs::write(&from, b"x").unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn({
            let from = from.clone();
            move || {
                let handle = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(from)
                    .unwrap();
                ready_tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                drop(handle);
            }
        });
        ready_rx.recv().unwrap();
        let started = std::time::Instant::now();
        let result = rename_with_retry(&from, &to);
        holder.join().unwrap();
        assert!(result.is_ok(), "first retry call failed: {result:?}");
        assert!(started.elapsed() >= std::time::Duration::from_millis(250));
        assert!(to.exists());
    }

    #[cfg(windows)]
    #[test]
    fn retry_after_permanent_exclusive_open_is_bounded_to_five_seconds() {
        use std::os::windows::fs::OpenOptionsExt;
        let target = tempfile::tempdir().unwrap();
        let from = target.path().join("held.exe");
        let to = target.path().join("moved.exe");
        fs::write(&from, b"x").unwrap();
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&from)
            .unwrap();
        let started = std::time::Instant::now();
        let result = rename_with_retry(&from, &to);
        drop(handle);
        assert!(result.is_err());
        assert!(started.elapsed() >= std::time::Duration::from_secs(3));
        assert!(started.elapsed() <= std::time::Duration::from_secs(8));
    }

    #[cfg(windows)]
    #[test]
    fn restart_manager_reports_child_and_install_refuses_replacement() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};

        let source_one = tempfile::tempdir().unwrap();
        fs::write(source_one.path().join("game.exe"), b"v1").unwrap();
        let source_two = tempfile::tempdir().unwrap();
        fs::write(source_two.path().join("game.exe"), b"v2").unwrap();
        let build_one = tempfile::tempdir().unwrap();
        let build_two = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let manifest_one = build_from_directory(
            source_one.path(),
            build_one.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let manifest_two = build_from_directory(
            source_two.path(),
            build_two.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            params,
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let target = tempfile::tempdir().unwrap();
        let installer = Installer::new(target.path());
        installer
            .install(
                &manifest_one,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    build_one.path().to_path_buf(),
                ))
                .unwrap(),
            )
            .unwrap();
        let target_file = target.path().join("game.exe");
        let escaped = target_file.to_string_lossy().replace('\'', "''");
        let command = format!(
            "$f=[IO.File]::Open('{escaped}','Open','Read','None'); Write-Output READY; Start-Sleep 30"
        );
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-Command", &command])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert!(
            ready.contains("READY"),
            "holder did not report readiness: {ready:?}"
        );
        let pid = child.id();
        let processes = restart_manager_processes(std::slice::from_ref(&target_file)).unwrap();
        let original = b"v1".to_vec();
        let install_result = installer.install(
            &manifest_two,
            &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                build_two.path().to_path_buf(),
            ))
            .unwrap(),
        );
        let _ = child.kill();
        let _ = child.wait();
        let unchanged = fs::read(&target_file).unwrap();
        assert!(processes.iter().any(|process| process.pid == pid));
        let error = install_result.unwrap_err().to_string();
        assert!(
            error.contains("target is in use by process"),
            "unexpected error: {error}"
        );
        assert_eq!(unchanged, original);
    }

    #[test]
    fn disk_full_injection_keeps_old_version() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("game.exe"), b"old").unwrap();
        let built = tempfile::tempdir().unwrap();
        let manifest = build_from_directory(
            source.path(),
            built.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let target = tempfile::tempdir().unwrap();
        let installer = Installer::new(target.path());
        installer
            .install(
                &manifest,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    built.path().to_path_buf(),
                ))
                .unwrap(),
            )
            .unwrap();
        fs::write(source.path().join("game.exe"), b"new").unwrap();
        let built2 = tempfile::tempdir().unwrap();
        let manifest2 = build_from_directory(
            source.path(),
            built2.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let failed = Installer::new(target.path())
            .with_hooks(InstallHooks {
                fail_staging_writes: true,
                ..InstallHooks::default()
            })
            .install(
                &manifest2,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    built2.path().to_path_buf(),
                ))
                .unwrap(),
            );
        assert!(failed.is_err());
        assert_eq!(fs::read(target.path().join("game.exe")).unwrap(), b"old");
    }

    #[test]
    fn committed_before_and_after_recovery_are_driven_by_install_hooks() {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("game.exe"), b"v1").unwrap();
        let build1 = tempfile::tempdir().unwrap();
        let m1 = build_from_directory(
            input.path(),
            build1.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        fs::write(input.path().join("game.exe"), b"v2").unwrap();
        let build2 = tempfile::tempdir().unwrap();
        let m2 = build_from_directory(
            input.path(),
            build2.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
            1024 * 1024,
        )
        .unwrap()
        .manifest;
        let target = tempfile::tempdir().unwrap();
        let base =
            vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(build1.path().to_path_buf()))
                .unwrap();
        Installer::new(target.path()).install(&m1, &base).unwrap();
        let before = Installer::new(target.path()).with_hooks(InstallHooks {
            fail_before_commit: true,
            ..InstallHooks::default()
        });
        assert!(before
            .install(
                &m2,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    build2.path().to_path_buf()
                ))
                .unwrap()
            )
            .is_err());
        before.recover().unwrap();
        assert_eq!(fs::read(target.path().join("game.exe")).unwrap(), b"v1");
        let after = Installer::new(target.path()).with_hooks(InstallHooks {
            fail_after_commit: true,
            ..InstallHooks::default()
        });
        assert!(after
            .install(
                &m2,
                &vesper_fetch::Fetcher::new(vesper_fetch::Source::Local(
                    build2.path().to_path_buf()
                ))
                .unwrap()
            )
            .is_err());
        after.recover().unwrap();
        assert_eq!(fs::read(target.path().join("game.exe")).unwrap(), b"v2");
    }

    #[derive(Clone, Debug)]
    struct RangeRequest {
        at: Instant,
        start: u64,
        end: u64,
    }

    #[derive(Clone, Debug)]
    struct RangeServerOptions {
        delay: Duration,
        bytes_per_second: Option<u64>,
        status: Option<u16>,
        fail_first_only: bool,
        status_delay: Duration,
        stall_first: bool,
        stall_chunk_delay: Duration,
    }

    impl Default for RangeServerOptions {
        fn default() -> Self {
            Self {
                delay: Duration::ZERO,
                bytes_per_second: None,
                status: None,
                fail_first_only: false,
                status_delay: Duration::ZERO,
                stall_first: false,
                stall_chunk_delay: Duration::from_millis(100),
            }
        }
    }

    struct RangeServer {
        base: String,
        requests: Arc<Mutex<Vec<RangeRequest>>>,
        bytes_served: Arc<Mutex<u64>>,
        max_active: Arc<std::sync::atomic::AtomicUsize>,
        interrupted: Arc<std::sync::atomic::AtomicUsize>,
        stop: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl RangeServer {
        fn new(pack_id: String, bytes: Vec<u8>, delay: Duration) -> Self {
            Self::with_options(
                pack_id,
                bytes,
                RangeServerOptions {
                    delay,
                    ..RangeServerOptions::default()
                },
            )
        }

        fn with_options(pack_id: String, bytes: Vec<u8>, options: RangeServerOptions) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let bytes_served = Arc::new(Mutex::new(0));
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let direct_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let interrupted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let thread_requests = Arc::clone(&requests);
            let thread_bytes_served = Arc::clone(&bytes_served);
            let thread_active = Arc::clone(&active);
            let thread_max_active = Arc::clone(&max_active);
            let thread_direct_count = Arc::clone(&direct_count);
            let thread_interrupted = Arc::clone(&interrupted);
            let thread_stop = Arc::clone(&stop);
            let thread_pack = pack_id.clone();
            let thread_bytes = Arc::new(bytes);
            let accept_bytes = Arc::clone(&thread_bytes);
            let thread_options = options.clone();
            let join = thread::spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    let Ok((stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    let requests = Arc::clone(&thread_requests);
                    let bytes_served = Arc::clone(&thread_bytes_served);
                    let active = Arc::clone(&thread_active);
                    let max_active = Arc::clone(&thread_max_active);
                    let bytes = Arc::clone(&accept_bytes);
                    let direct_count = Arc::clone(&thread_direct_count);
                    let interrupted = Arc::clone(&thread_interrupted);
                    let pack = thread_pack.clone();
                    let options = thread_options.clone();
                    thread::spawn(move || {
                        handle_range_request(
                            stream,
                            &pack,
                            &bytes,
                            &options,
                            requests,
                            bytes_served,
                            active,
                            max_active,
                            direct_count,
                            interrupted,
                        );
                    });
                }
            });
            thread::sleep(Duration::from_millis(10));
            Self {
                base,
                requests,
                bytes_served,
                max_active,
                interrupted,
                stop,
                join: Some(join),
            }
        }

        fn direct_requests(&self) -> Vec<RangeRequest> {
            self.requests.lock().unwrap().clone()
        }

        fn max_concurrency(&self) -> usize {
            self.max_active.load(Ordering::SeqCst)
        }

        fn bytes_served(&self) -> u64 {
            *self.bytes_served.lock().unwrap()
        }

        fn interrupted_connections(&self) -> usize {
            self.interrupted.load(Ordering::SeqCst)
        }
    }

    impl Drop for RangeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_range_request(
        mut stream: TcpStream,
        pack_id: &str,
        bytes: &[u8],
        options: &RangeServerOptions,
        requests: Arc<Mutex<Vec<RangeRequest>>>,
        bytes_served: Arc<Mutex<u64>>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        max_active: Arc<std::sync::atomic::AtomicUsize>,
        direct_count: Arc<std::sync::atomic::AtomicUsize>,
        interrupted: Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let _ = stream.set_nonblocking(false);
        let mut request = Vec::new();
        let mut buffer = [0u8; 2048];
        loop {
            let Ok(read) = stream.read(&mut buffer) else {
                return;
            };
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&request);
        let path = text
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("");
        if path.starts_with("/packs/") {
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: /direct/{pack_id}?X-Amz-Expires=60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(Shutdown::Write);
            return;
        }
        if !path.starts_with("/direct/") {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let range = text
            .lines()
            .find_map(|line| {
                line.strip_prefix("Range: bytes=")
                    .or_else(|| line.strip_prefix("range: bytes="))
            })
            .and_then(|value| {
                let mut parts = value.split('-');
                Some((
                    parts.next()?.parse::<u64>().ok()?,
                    parts.next()?.parse::<u64>().ok()?,
                ))
            });
        let Some((start, end)) = range else { return };
        if start >= bytes.len() as u64 {
            let _ = stream.write_all(
                b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.shutdown(Shutdown::Write);
            return;
        }
        let end = end.min(bytes.len().saturating_sub(1) as u64);
        let payload = &bytes[start as usize..=end as usize];
        let ordinal = direct_count.fetch_add(1, Ordering::SeqCst);
        let now = Instant::now();
        requests.lock().unwrap().push(RangeRequest {
            at: now,
            start,
            end,
        });
        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
        max_active.fetch_max(count, Ordering::SeqCst);
        if options.delay > Duration::ZERO {
            thread::sleep(options.delay);
        }
        let should_fail = options.status.is_some() && (!options.fail_first_only || ordinal == 0);
        if should_fail {
            if options.status_delay > Duration::ZERO {
                thread::sleep(options.status_delay);
            }
            let status = options.status.unwrap_or(500);
            let response = format!(
                "HTTP/1.1 {status} Test Failure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(response.as_bytes()).is_err() {
                interrupted.fetch_add(1, Ordering::SeqCst);
            }
            let _ = stream.shutdown(Shutdown::Write);
            active.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        let header = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len(),
            payload.len()
        );
        if stream.write_all(header.as_bytes()).is_err() {
            interrupted.fetch_add(1, Ordering::SeqCst);
            active.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        let chunk_size = if options.stall_first && ordinal == 0 {
            1024
        } else {
            64 * 1024
        };
        for chunk in payload.chunks(chunk_size) {
            if stream.write_all(chunk).is_err() {
                interrupted.fetch_add(1, Ordering::SeqCst);
                active.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            *bytes_served.lock().unwrap() += chunk.len() as u64;
            let sleep = if options.stall_first && ordinal == 0 {
                options.stall_chunk_delay
            } else if let Some(rate) = options.bytes_per_second {
                Duration::from_secs_f64(chunk.len() as f64 / rate.max(1) as f64)
            } else {
                Duration::ZERO
            };
            if sleep > Duration::ZERO {
                thread::sleep(sleep);
            }
        }
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Write);
        active.fetch_sub(1, Ordering::SeqCst);
    }

    fn http_install_fixture(
        file_count: usize,
        file_size: usize,
        delay: Duration,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        vesper_format::BuildOutput,
        RangeServer,
    ) {
        http_install_fixture_with_options(
            file_count,
            file_size,
            RangeServerOptions {
                delay,
                ..RangeServerOptions::default()
            },
        )
    }

    fn http_install_fixture_with_options(
        file_count: usize,
        file_size: usize,
        options: RangeServerOptions,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        vesper_format::BuildOutput,
        RangeServer,
    ) {
        let input = tempfile::tempdir().unwrap();
        for index in 0..file_count {
            let mut bytes = vec![0u8; file_size];
            let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ index as u64;
            for (offset, value) in bytes.iter_mut().enumerate() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1 + offset as u64);
                *value = (state >> 32) as u8;
            }
            fs::write(input.path().join(format!("file-{index:03}.bin")), bytes).unwrap();
        }
        let build = tempfile::tempdir().unwrap();
        let output = build_from_directory(
            input.path(),
            build.path(),
            "scheduler-test".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64 * 1024,
                avg_size: 128 * 1024,
                max_size: 256 * 1024,
            },
            256 * 1024 * 1024,
        )
        .unwrap();
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(build.path().join("packs").join(&pack_id)).unwrap();
        let server = RangeServer::with_options(pack_id, pack, options);
        (input, build, output, server)
    }

    fn manifest_pack_size(build: &tempfile::TempDir, manifest: &Manifest) -> u64 {
        manifest
            .packs
            .iter()
            .map(|pack| {
                fs::metadata(build.path().join("packs").join(&pack.pack_id))
                    .unwrap()
                    .len()
            })
            .sum()
    }

    #[test]
    fn hedge_never_fires_on_uniform_healthy_sources() {
        let (_input, build, output, primary) = http_install_fixture_with_options(
            1,
            160 * 1024 * 1024,
            RangeServerOptions {
                delay: Duration::from_millis(50),
                bytes_per_second: Some(4 * 1024 * 1024),
                ..RangeServerOptions::default()
            },
        );
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(build.path().join("packs").join(&pack_id)).unwrap();
        let backup = RangeServer::with_options(
            pack_id,
            pack,
            RangeServerOptions {
                delay: Duration::from_millis(50),
                bytes_per_second: Some(4 * 1024 * 1024),
                ..RangeServerOptions::default()
            },
        );
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(primary.base.clone())),
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(backup.base.clone())),
        ])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        let required = manifest_pack_size(&build, &output.manifest);
        let served = primary.bytes_served() + backup.bytes_served();
        assert!(
            served <= (required as f64 * 1.02).ceil() as u64,
            "uniform sources duplicated bytes: served={served} required={required}"
        );
        assert_eq!(fetcher.hedged_bytes(), 0);
    }

    #[test]
    fn single_healthy_source_has_no_duplicate_bytes() {
        let (_input, build, output, server) = http_install_fixture_with_options(
            1,
            160 * 1024 * 1024,
            RangeServerOptions {
                delay: Duration::from_millis(50),
                bytes_per_second: Some(4 * 1024 * 1024),
                ..RangeServerOptions::default()
            },
        );
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![vesper_fetch::SourceSpec::new(
            vesper_fetch::Source::Http(server.base.clone()),
        )])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        let required = manifest_pack_size(&build, &output.manifest);
        let served = server.bytes_served();
        assert!(
            served <= (required as f64 * 1.02).ceil() as u64,
            "single source duplicated bytes: served={served} required={required}"
        );
        assert_eq!(fetcher.hedged_bytes(), 0);
    }

    #[test]
    fn stalled_request_is_hedged_and_loser_cancelled() {
        let (_input, build, output, slow) = http_install_fixture_with_options(
            4,
            8 * 1024 * 1024,
            RangeServerOptions {
                stall_first: true,
                stall_chunk_delay: Duration::from_millis(100),
                ..RangeServerOptions::default()
            },
        );
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(build.path().join("packs").join(&pack_id)).unwrap();
        let fast = RangeServer::new(pack_id, pack, Duration::ZERO);
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(slow.base.clone())),
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(fast.base.clone())),
        ])
        .unwrap();
        let started = Instant::now();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(150));
        assert!(
            slow.interrupted_connections() > 0,
            "stalled loser was not cancelled: requests={} bytes={}",
            slow.direct_requests().len(),
            slow.bytes_served()
        );
        assert!(fetcher.hedged_bytes() <= 16 * 1024 * 1024);
    }

    #[test]
    fn hedge_success_survives_original_failure() {
        let (_input, build, output, primary) = http_install_fixture_with_options(
            4,
            8 * 1024 * 1024,
            RangeServerOptions {
                status: Some(500),
                fail_first_only: true,
                status_delay: Duration::from_millis(2500),
                ..RangeServerOptions::default()
            },
        );
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(build.path().join("packs").join(&pack_id)).unwrap();
        let backup = RangeServer::new(pack_id, pack, Duration::from_millis(2500));
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(primary.base.clone())),
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(backup.base.clone())),
        ])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        assert!(!primary.direct_requests().is_empty());
        assert!(!backup.direct_requests().is_empty());
    }

    #[test]
    fn failing_primary_is_demoted_after_three_errors() {
        let (_input, build, output, primary) = http_install_fixture_with_options(
            8,
            4 * 1024 * 1024,
            RangeServerOptions {
                status: Some(503),
                ..RangeServerOptions::default()
            },
        );
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(build.path().join("packs").join(&pack_id)).unwrap();
        let backup = RangeServer::new(pack_id, pack, Duration::from_millis(50));
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(primary.base.clone())),
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(backup.base.clone())),
        ])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        assert!(
            primary.direct_requests().len() <= 3,
            "demoted primary received {} requests",
            primary.direct_requests().len()
        );
        assert!(!backup.direct_requests().is_empty());
    }

    #[test]
    fn c3a_installer_merges_adjacent_chunks_across_files() {
        let (_input, build, output, server) = http_install_fixture(64, 64, Duration::ZERO);
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![vesper_fetch::SourceSpec::new(
            vesper_fetch::Source::Http(server.base.clone()),
        )])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        let ids = output
            .manifest
            .files
            .iter()
            .flat_map(|file| file.chunks.iter().map(|chunk| chunk.id))
            .collect::<Vec<_>>();
        let theory = vesper_fetch::plan_ranges(&output.manifest, &ids)
            .unwrap()
            .len();
        assert!(server.direct_requests().len() <= theory);
        assert!(Installer::new(target.path())
            .verify(&output.manifest)
            .unwrap()
            .is_empty());
        let _ = build;
    }

    #[test]
    fn c3b_installer_observes_six_to_eight_parallel_ranges() {
        let (_input, _build, output, server) =
            http_install_fixture(32, 4 * 1024 * 1024, Duration::from_millis(40));
        assert_eq!(output.manifest.packs.len(), 1);
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![vesper_fetch::SourceSpec::new(
            vesper_fetch::Source::Http(server.base.clone()),
        )])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        assert!(
            server.max_concurrency() >= 6,
            "max={} requests={}",
            server.max_concurrency(),
            server.direct_requests().len()
        );
        assert!(server.max_concurrency() <= 8);
    }

    #[test]
    fn c3c_installer_uses_one_healthy_source_before_the_backup() {
        let (_input, _build, output, primary) = http_install_fixture(16, 64 * 1024, Duration::ZERO);
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(_build.path().join("packs").join(&pack_id)).unwrap();
        let backup = RangeServer::new(pack_id, pack, Duration::ZERO);
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(primary.base.clone())),
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(backup.base.clone())),
        ])
        .unwrap();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        let required = primary.bytes_served();
        assert!(backup.bytes_served() <= required / 10 + 1);
    }

    #[test]
    fn c3d_installer_hedges_only_after_queue_drains() {
        let (_input, _build, output, slow) =
            http_install_fixture(1, 512 * 1024, Duration::from_millis(800));
        let pack_id = output.manifest.packs[0].pack_id.clone();
        let pack = fs::read(_build.path().join("packs").join(&pack_id)).unwrap();
        let fast = RangeServer::new(pack_id, pack, Duration::ZERO);
        let target = tempfile::tempdir().unwrap();
        let fetcher = vesper_fetch::MultiSourceFetcher::new(vec![
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(slow.base.clone())),
            vesper_fetch::SourceSpec::new(vesper_fetch::Source::Http(fast.base.clone())),
        ])
        .unwrap();
        let started = Instant::now();
        Installer::new(target.path())
            .install(&output.manifest, &fetcher)
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let duplicates = slow.direct_requests().len() + fast.direct_requests().len();
        assert!(duplicates <= 8);
        let slow_requests = slow.direct_requests();
        let fast_requests = fast.direct_requests();
        if let (Some(slow_first), Some(fast_first)) = (slow_requests.first(), fast_requests.first())
        {
            assert!(fast_first.at >= slow_first.at);
        }
        assert!(slow_requests
            .iter()
            .all(|request| request.end >= request.start));
    }
}
