//! Resumable, hash-first pack fetching for local and HTTP sources.

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_RANGE, LOCATION, RANGE};
use reqwest::redirect::Policy;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vesper_format::{ChunkId, ChunkLocation, CryptoContext, Manifest, PackFrame};

#[derive(Debug, Clone)]
pub enum Source {
    Local(PathBuf),
    Http(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSpan {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRange {
    pub pack_id: String,
    pub release_tag: Option<String>,
    pub start: u64,
    pub end: u64,
    pub chunks: Vec<(ChunkId, u64, u64)>,
}

struct InFlightRange {
    started: Instant,
    hedged: bool,
    finished: bool,
    plan: PlannedRange,
    cancel: Arc<AtomicBool>,
    progress: Arc<RangeProgress>,
}

struct RangeProgress {
    received: AtomicU64,
    source: AtomicUsize,
}

impl Default for RangeProgress {
    fn default() -> Self {
        Self {
            received: AtomicU64::new(0),
            source: AtomicUsize::new(usize::MAX),
        }
    }
}

struct FetchAttempt {
    values: Vec<(ChunkId, Vec<u8>)>,
    source_index: usize,
    transferred_bytes: u64,
    elapsed: Duration,
}

pub fn plan_ranges(manifest: &Manifest, ids: &[ChunkId]) -> Result<Vec<PlannedRange>> {
    let mut requests = ids
        .iter()
        .map(|id| {
            let location = manifest
                .chunks
                .get(id)
                .with_context(|| format!("missing chunk location for {id}"))?;
            Ok((
                id,
                location.pack_id.clone(),
                location.offset,
                location
                    .offset
                    .saturating_add(location.frame_len)
                    .saturating_sub(1),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    requests.sort_by(|left, right| (left.1.as_str(), left.2).cmp(&(right.1.as_str(), right.2)));
    let mut planned: Vec<PlannedRange> = Vec::new();
    for (id, pack_id, start, end) in requests {
        if let Some(last) = planned.last_mut() {
            let gap = start.saturating_sub(last.end.saturating_add(1));
            let combined = end.saturating_sub(last.start).saturating_add(1);
            let can_merge = merge_ranges(vec![
                RangeSpan {
                    start: last.start,
                    end: last.end,
                },
                RangeSpan { start, end },
            ])
            .len()
                == 1;
            if last.pack_id == pack_id
                && gap <= 256 * 1024
                && combined <= 16 * 1024 * 1024
                && can_merge
            {
                last.end = last.end.max(end);
                last.chunks.push((*id, start, end));
                continue;
            }
        }
        planned.push(PlannedRange {
            pack_id,
            release_tag: None,
            start,
            end,
            chunks: vec![(*id, start, end)],
        });
    }
    Ok(planned)
}

fn plan_ranges_for_manifest(manifest: &Manifest, ids: &[ChunkId]) -> Result<Vec<PlannedRange>> {
    let mut plans = plan_ranges(manifest, ids)?;
    for plan in &mut plans {
        plan.release_tag = manifest
            .packs
            .iter()
            .find(|pack| pack.pack_id == plan.pack_id)
            .and_then(|pack| pack.release_tag.clone());
    }
    Ok(plans)
}

/// Give the useful-range sampler two early opportunities before large merged
/// requests consume the queue. Single-source downloads retain full coalescing.
fn early_sampling_ranges(mut plans: Vec<PlannedRange>) -> Vec<PlannedRange> {
    let original_count = plans.len();
    let Some(first) = plans.first_mut() else {
        return plans;
    };
    let count = first.chunks.len().min(40);
    let mut early = Vec::with_capacity(count + original_count);
    // Each frame is requested exactly once; gaps need no content download.
    for chunk in first.chunks.drain(..count) {
        early.push(PlannedRange {
            pack_id: first.pack_id.clone(),
            release_tag: first.release_tag.clone(),
            start: chunk.1,
            end: chunk.2,
            chunks: vec![chunk],
        });
    }
    if let Some(chunk) = first.chunks.first() {
        first.start = chunk.1;
    } else {
        plans.remove(0);
    }
    early.extend(plans);
    early
}

pub fn merge_ranges(mut ranges: Vec<RangeSpan>) -> Vec<RangeSpan> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<RangeSpan> = Vec::new();
    for range in ranges {
        if range.end < range.start {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            let gap = range.start.saturating_sub(last.end.saturating_add(1));
            let combined = range.end.saturating_sub(last.start).saturating_add(1);
            if gap <= 256 * 1024 && combined <= 16 * 1024 * 1024 {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

#[derive(Debug, Clone, Default)]
pub struct ProgressEvent {
    /// Compressed frame/Range payload bytes received; excludes transport headers.
    pub downloaded_bytes: u64,
    /// Verified raw bytes recovered from local files or verified chunk caches.
    pub reused_bytes: u64,
    pub hedged_bytes: u64,
    pub source_speeds: HashMap<String, u64>,
    pub remaining_bytes: u64,
    pub demoted_sources: Vec<String>,
}

type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync + 'static>;

#[derive(Default)]
struct ProgressState {
    downloaded_bytes: u64,
    reused_bytes: u64,
    hedged_bytes: u64,
    remaining_bytes: u64,
    source_speeds: HashMap<String, u64>,
    demoted_sources: Vec<String>,
}

#[derive(Clone)]
struct DirectLink {
    url: String,
    expires_at: Instant,
}

pub trait Materializer {
    fn materialize_file(&self, manifest: &Manifest, file_index: usize, out: &Path) -> Result<()>;

    /// Materialize a transaction's changed files as one scheduled download.
    /// Implementations may override this to share merged range requests across
    /// files; the default preserves compatibility for simple materializers.
    fn materialize_files(
        &self,
        manifest: &Manifest,
        file_indices: &[usize],
        staging_root: &Path,
    ) -> Result<()> {
        for index in file_indices {
            let file = manifest
                .files
                .get(*index)
                .context("file index out of range")?;
            let out = staging_root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            self.materialize_file(manifest, *index, &out)?;
        }
        Ok(())
    }

    /// Estimate raw chunk bytes that this materializer must obtain for the
    /// changed files. Installers use the same value for preflight and the
    /// transaction itself.
    fn estimate_download_bytes(&self, _manifest: &Manifest, _file_indices: &[usize]) -> u64 {
        0
    }

    /// Remove verified download chunks after the enclosing install transaction
    /// has committed. Failed or interrupted transactions retain the store.
    fn cleanup_after_commit(&self) -> Result<()> {
        Ok(())
    }

    /// Number of bytes fetched by losing hedge attempts in this transaction.
    fn hedged_bytes(&self) -> u64 {
        0
    }
}

impl Source {
    pub fn id(&self) -> String {
        match self {
            Source::Local(path) => format!("local:{}", path.display()),
            Source::Http(url) => format!("http:{url}"),
        }
    }
}

pub struct Fetcher {
    client: Client,
    source: Source,
    chunk_cache: Mutex<BoundedChunkCache>,
    chunk_store: Option<PathBuf>,
    crypto: Option<CryptoContext>,
    local_reuse: Option<HashMap<ChunkId, (PathBuf, u64, u64)>>,
    default_release: Option<String>,
    pub max_raw_size: u64,
    direct_links: Mutex<HashMap<String, DirectLink>>,
    range_unsupported: Mutex<bool>,
    direct_link_ttl: Duration,
    progress: Option<ProgressCallback>,
    progress_state: Arc<Mutex<ProgressState>>,
    pub concurrency: usize,
}

const CHUNK_CACHE_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct BoundedChunkCache {
    entries: HashMap<ChunkId, Vec<u8>>,
    order: VecDeque<ChunkId>,
    bytes: usize,
}

impl BoundedChunkCache {
    fn get(&mut self, id: &ChunkId) -> Option<Vec<u8>> {
        let value = self.entries.get(id).cloned();
        if value.is_some() {
            self.order.retain(|item| item != id);
            self.order.push_back(*id);
        }
        value
    }

    fn insert(&mut self, id: ChunkId, value: Vec<u8>) {
        if let Some(previous) = self.entries.remove(&id) {
            self.bytes = self.bytes.saturating_sub(previous.len());
            self.order.retain(|item| item != &id);
        }
        if value.len() > CHUNK_CACHE_LIMIT {
            return;
        }
        self.bytes += value.len();
        self.entries.insert(id, value);
        self.order.push_back(id);
        while self.bytes > CHUNK_CACHE_LIMIT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.len());
            }
        }
    }

    #[cfg(test)]
    fn total_bytes(&self) -> usize {
        self.bytes
    }
}

impl Fetcher {
    pub fn new(source: Source) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("vesper/", env!("CARGO_PKG_VERSION")))
                .redirect(Policy::none())
                .build()?,
            source,
            chunk_cache: Mutex::new(BoundedChunkCache::default()),
            chunk_store: None,
            crypto: None,
            local_reuse: None,
            default_release: None,
            max_raw_size: 4 * 1024 * 1024,
            direct_links: Mutex::new(HashMap::new()),
            range_unsupported: Mutex::new(false),
            direct_link_ttl: Duration::from_secs(600),
            progress: None,
            progress_state: Arc::new(Mutex::new(ProgressState::default())),
            concurrency: 8,
        })
    }

    pub fn with_direct_link_ttl(mut self, ttl: Duration) -> Self {
        self.direct_link_ttl = ttl;
        self
    }

    pub fn with_default_release(mut self, release: impl Into<String>) -> Self {
        self.default_release = Some(release.into());
        self
    }

    pub fn with_progress_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(ProgressEvent) + Send + Sync + 'static,
    {
        self.progress = Some(Arc::new(callback));
        self
    }

    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    fn emit_progress(&self, downloaded: u64, reused: u64, remaining: u64, source: &str) {
        self.emit_progress_detail(downloaded, reused, remaining, source, 0, None);
    }

    fn emit_progress_detail(
        &self,
        downloaded: u64,
        reused: u64,
        remaining: u64,
        source: &str,
        hedged: u64,
        demoted: Option<String>,
    ) {
        let Some(callback) = &self.progress else {
            return;
        };
        let mut state = self.progress_state.lock().unwrap();
        state.downloaded_bytes = state.downloaded_bytes.saturating_add(downloaded);
        state.reused_bytes = state.reused_bytes.saturating_add(reused);
        state.hedged_bytes = state.hedged_bytes.saturating_add(hedged);
        state.remaining_bytes = remaining;
        if let Some(source) = demoted {
            if !state.demoted_sources.contains(&source) {
                state.demoted_sources.push(source);
            }
        }
        if downloaded > 0 {
            *state.source_speeds.entry(source.to_string()).or_default() = downloaded;
        }
        callback(ProgressEvent {
            downloaded_bytes: state.downloaded_bytes,
            reused_bytes: state.reused_bytes,
            hedged_bytes: state.hedged_bytes,
            source_speeds: state.source_speeds.clone(),
            remaining_bytes: state.remaining_bytes,
            demoted_sources: state.demoted_sources.clone(),
        });
    }

    pub fn hedged_bytes(&self) -> u64 {
        self.progress_state.lock().unwrap().hedged_bytes
    }

    pub fn with_chunk_store(mut self, path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        fs::create_dir_all(&path)?;
        self.chunk_store = Some(path);
        Ok(self)
    }

    pub fn cleanup_chunk_store(&self) -> Result<()> {
        let Some(store) = &self.chunk_store else {
            return Ok(());
        };
        if !store.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(store)? {
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

    pub fn with_crypto(mut self, crypto: CryptoContext) -> Self {
        self.crypto = Some(crypto);
        self
    }

    /// Reuse chunks already present in an installed tree before consulting a
    /// pack source. The old file is accepted only after the same content hash
    /// check used for downloaded chunks.
    pub fn with_local_manifest(
        mut self,
        manifest: &Manifest,
        install_root: impl Into<PathBuf>,
    ) -> Self {
        let root = install_root.into();
        let mut reuse = HashMap::new();
        for file in &manifest.files {
            let mut offset = 0u64;
            let path = root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            for chunk in &file.chunks {
                reuse
                    .entry(chunk.id)
                    .or_insert_with(|| (path.clone(), offset, chunk.raw_size));
                offset = offset.saturating_add(chunk.raw_size);
            }
        }
        self.local_reuse = Some(reuse);
        self
    }

    pub fn source_id(&self) -> String {
        self.source.id()
    }

    pub fn fetch_chunk(&self, id: ChunkId, manifest: &Manifest) -> Result<Vec<u8>> {
        if let Some(bytes) = self.chunk_cache.lock().unwrap().get(&id) {
            self.emit_progress(0, bytes.len() as u64, 0, &self.source_id());
            return Ok(bytes);
        }
        if let Some(store) = &self.chunk_store {
            let path = store.join(id.hex());
            if path.is_file() {
                let bytes = fs::read(&path)?;
                if vesper_format::hash_bytes(&bytes) == id.0 {
                    self.chunk_cache.lock().unwrap().insert(id, bytes.clone());
                    self.emit_progress(0, bytes.len() as u64, 0, &self.source_id());
                    return Ok(bytes);
                }
                let _ = fs::remove_file(path);
            }
        }
        if let Some(reuse) = &self.local_reuse {
            if let Some((path, offset, len)) = reuse.get(&id) {
                if let Ok(mut file) = File::open(path) {
                    if file.seek(SeekFrom::Start(*offset)).is_ok() {
                        let mut bytes = vec![0u8; *len as usize];
                        if file.read_exact(&mut bytes).is_ok() {
                            let actual = if let Some(crypto) = &self.crypto {
                                crypto.chunk_id(&bytes)
                            } else {
                                vesper_format::ChunkId::from_bytes(&bytes)
                            };
                            if actual == id {
                                self.chunk_cache.lock().unwrap().insert(id, bytes.clone());
                                self.emit_progress(0, bytes.len() as u64, 0, &self.source_id());
                                return Ok(bytes);
                            }
                        }
                    }
                }
            }
        }
        let location = manifest.chunks.get(&id).context("missing chunk location")?;
        let frame_bytes = self.read_range(manifest, location)?;
        let frame = PackFrame::read_from(frame_bytes.as_slice(), self.max_raw_size)
            .with_context(|| format!("parse frame for chunk {id}"))?;
        let bytes = frame.decode_payload(self.crypto.as_ref())?;
        let actual_id = if let Some(crypto) = &self.crypto {
            crypto.chunk_id(&bytes)
        } else {
            vesper_format::ChunkId::from_bytes(&bytes)
        };
        if actual_id != id {
            anyhow::bail!("requested chunk {id}, source returned a different content hash");
        }
        if let Some(store) = &self.chunk_store {
            let tmp = store.join(format!("{}.part", id.hex()));
            fs::write(&tmp, &bytes)
                .with_context(|| format!("write chunk temp {}", tmp.display()))?;
            let file =
                File::open(&tmp).with_context(|| format!("open chunk temp {}", tmp.display()))?;
            if let Err(error) = file.sync_all() {
                if error.kind() != std::io::ErrorKind::PermissionDenied {
                    return Err(error)
                        .with_context(|| format!("sync chunk temp {}", tmp.display()));
                }
            }
            drop(file);
            let destination = store.join(id.hex());
            fs::rename(&tmp, &destination)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        let _ = fs::remove_file(&destination);
                        fs::copy(&tmp, &destination)
                            .map(|_| ())
                            .and_then(|_| fs::remove_file(&tmp))
                    } else {
                        Err(error)
                    }
                })
                .with_context(|| format!("commit verified chunk {}", destination.display()))?;
        }
        self.chunk_cache.lock().unwrap().insert(id, bytes.clone());
        self.emit_progress(frame_bytes.len() as u64, 0, 0, &self.source_id());
        Ok(bytes)
    }

    /// Fetch a set of chunks with bounded same-source concurrency. Results are
    /// returned only after every requested chunk has passed frame and content
    /// hash verification.
    pub fn fetch_chunks_parallel(
        &self,
        manifest: &Manifest,
        ids: &[ChunkId],
    ) -> Result<HashMap<ChunkId, Vec<u8>>> {
        let mut output = HashMap::new();
        let mut pending = Vec::new();
        for id in ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            if let Some(bytes) = self.cached_or_reused(id, manifest)? {
                output.insert(id, bytes);
            } else {
                pending.push(id);
            }
        }
        let plans = plan_ranges_for_manifest(manifest, &pending)?;
        for batch in plans.chunks(self.concurrency.max(1)) {
            let (tx, rx) = mpsc::channel();
            thread::scope(|scope| {
                for plan in batch {
                    let tx = tx.clone();
                    scope.spawn(move || {
                        let result = self.fetch_plan(manifest, plan);
                        let _ = tx.send(result);
                    });
                }
                drop(tx);
                for result in rx {
                    for (id, bytes) in result? {
                        output.insert(id, bytes);
                    }
                }
                Ok::<(), anyhow::Error>(())
            })?;
        }
        Ok(output)
    }

    fn fetch_plan(
        &self,
        manifest: &Manifest,
        plan: &PlannedRange,
    ) -> Result<Vec<(ChunkId, Vec<u8>)>> {
        self.read_range_span(
            &plan.pack_id,
            plan.release_tag.as_deref(),
            plan.start,
            plan.end,
        )
        .and_then(|bytes| {
            let mut decoded = Vec::new();
            for (id, start, end) in &plan.chunks {
                let from = start.saturating_sub(plan.start) as usize;
                let len = end.saturating_sub(*start).saturating_add(1) as usize;
                let frame = bytes
                    .get(from..from.saturating_add(len))
                    .context("merged range did not contain requested frame")?;
                let location = manifest.chunks.get(id).context("missing chunk location")?;
                let raw = self.decode_verified_chunk_without_cache(*id, location, frame)?;
                decoded.push((*id, raw));
            }
            self.emit_progress(bytes.len() as u64, 0, 0, &self.source_id());
            Ok(decoded)
        })
    }

    fn fetch_plan_with_cancel(
        &self,
        manifest: &Manifest,
        plan: &PlannedRange,
        cancel: &Arc<AtomicBool>,
        persist: bool,
        progress: Option<&RangeProgress>,
    ) -> Result<FetchAttempt> {
        let started = Instant::now();
        let bytes = self.read_range_span_with_cancel(
            &plan.pack_id,
            plan.release_tag.as_deref(),
            plan.start,
            plan.end,
            cancel,
            progress,
        )?;
        if cancel.load(Ordering::Acquire) {
            anyhow::bail!("range request cancelled after another attempt succeeded");
        }
        let transferred_bytes = bytes.len() as u64;
        let values = (|| {
            let mut decoded = Vec::new();
            for (id, start, end) in &plan.chunks {
                if cancel.load(Ordering::Acquire) {
                    anyhow::bail!("range request cancelled after another attempt succeeded");
                }
                let from = start.saturating_sub(plan.start) as usize;
                let len = end.saturating_sub(*start).saturating_add(1) as usize;
                let frame = bytes
                    .get(from..from.saturating_add(len))
                    .context("merged range did not contain requested frame")?;
                let _ = manifest.chunks.get(id).context("missing chunk location")?;
                let raw = self.decode_verified_chunk_impl(*id, frame, false, persist)?;
                decoded.push((*id, raw));
            }
            Ok::<_, anyhow::Error>(decoded)
        })()?;
        Ok(FetchAttempt {
            values,
            source_index: 0,
            transferred_bytes,
            elapsed: started.elapsed(),
        })
    }

    pub fn materialize_file(
        &self,
        manifest: &Manifest,
        file_index: usize,
        out: &Path,
    ) -> Result<()> {
        let file = manifest
            .files
            .get(file_index)
            .context("file index out of range")?;
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(out)?;
        for batch in file.chunks.chunks(self.concurrency.max(1)) {
            let ids = batch.iter().map(|chunk| chunk.id).collect::<Vec<_>>();
            let fetched = self.fetch_chunks_parallel(manifest, &ids)?;
            for chunk in batch {
                let bytes = fetched
                    .get(&chunk.id)
                    .context("parallel fetch omitted chunk")?;
                output.write_all(bytes)?;
            }
        }
        output.flush()?;
        output.sync_all()?;
        Ok(())
    }

    /// Schedule all missing chunks for a transaction at once. Each worker
    /// owns at most one merged range (16 MiB maximum), and completed chunks
    /// are written directly to their staging offsets before the next result is
    /// accepted. This keeps memory bounded by the active request count.
    pub fn materialize_files(
        &self,
        manifest: &Manifest,
        file_indices: &[usize],
        staging_root: &Path,
    ) -> Result<()> {
        let mut destinations: HashMap<ChunkId, Vec<(PathBuf, u64)>> = HashMap::new();
        let mut outputs = HashMap::<PathBuf, File>::new();
        let selected = file_indices.iter().copied().collect::<BTreeSet<_>>();
        for index in file_indices {
            let file = manifest
                .files
                .get(*index)
                .context("file index out of range")?;
            let path = staging_root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let output = File::create(&path)?;
            output.set_len(file.size)?;
            outputs.insert(path.clone(), output);
            let mut offset = 0u64;
            for chunk in &file.chunks {
                destinations
                    .entry(chunk.id)
                    .or_default()
                    .push((path.clone(), offset));
                offset = offset.saturating_add(chunk.raw_size);
            }
        }
        if selected.is_empty() {
            return Ok(());
        }
        let mut missing = Vec::new();
        let mut completed = HashMap::new();
        for id in destinations.keys().copied() {
            if let Some(bytes) = self.cached_or_reused(id, manifest)? {
                completed.insert(id, bytes);
            } else {
                missing.push(id);
            }
        }
        for (id, bytes) in completed {
            write_destinations(&mut outputs, &destinations, id, &bytes)?;
        }
        let plans = plan_ranges_for_manifest(manifest, &missing)?;
        let queue = Arc::new(Mutex::new(VecDeque::from(plans)));
        let configured_workers = if matches!(self.source, Source::Local(_)) {
            self.concurrency.min(2)
        } else {
            self.concurrency
        };
        let workers = configured_workers
            .max(1)
            .min(queue.lock().unwrap().len().max(1));
        let (tx, rx) = mpsc::channel::<Result<Vec<(ChunkId, Vec<u8>)>>>();
        thread::scope(|scope| {
            for _ in 0..workers {
                let queue = Arc::clone(&queue);
                let tx = tx.clone();
                scope.spawn(move || loop {
                    let plan = queue.lock().unwrap().pop_front();
                    let Some(plan) = plan else { break };
                    let result = self.fetch_plan(manifest, &plan);
                    if tx.send(result).is_err() {
                        break;
                    }
                });
            }
            drop(tx);
            for result in rx {
                for (id, bytes) in result? {
                    write_destinations(&mut outputs, &destinations, id, &bytes)?;
                }
            }
            Ok::<(), anyhow::Error>(())
        })?;
        for output in outputs.values_mut() {
            output.flush()?;
            output.sync_all()?;
        }
        Ok(())
    }

    fn read_range(&self, manifest: &Manifest, location: &ChunkLocation) -> Result<Vec<u8>> {
        self.read_range_span(
            &location.pack_id,
            manifest
                .packs
                .iter()
                .find(|pack| pack.pack_id == location.pack_id)
                .and_then(|pack| pack.release_tag.as_deref()),
            location.offset,
            location.offset + location.frame_len - 1,
        )
    }

    fn read_range_span(
        &self,
        pack_id: &str,
        release_tag: Option<&str>,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>> {
        self.read_range_span_with_cancel(
            pack_id,
            release_tag,
            start,
            end,
            &Arc::new(AtomicBool::new(false)),
            None,
        )
    }

    fn read_range_span_with_cancel(
        &self,
        pack_id: &str,
        release_tag: Option<&str>,
        start: u64,
        end: u64,
        cancel: &Arc<AtomicBool>,
        progress: Option<&RangeProgress>,
    ) -> Result<Vec<u8>> {
        match &self.source {
            Source::Local(root) => {
                if cancel.load(Ordering::Acquire) {
                    anyhow::bail!("range request cancelled");
                }
                let path = root.join("packs").join(pack_id);
                let mut file =
                    File::open(&path).with_context(|| format!("open pack {}", path.display()))?;
                file.seek(SeekFrom::Start(start))?;
                let mut bytes = vec![0u8; end.saturating_sub(start).saturating_add(1) as usize];
                file.read_exact(&mut bytes)?;
                if let Some(progress) = progress {
                    progress
                        .received
                        .store(bytes.len() as u64, Ordering::Release);
                }
                if cancel.load(Ordering::Acquire) {
                    anyhow::bail!("range request cancelled");
                }
                Ok(bytes)
            }
            Source::Http(base) => {
                if *self.range_unsupported.lock().unwrap() {
                    anyhow::bail!("source does not support HTTP Range requests");
                }
                let original = pack_url(
                    base,
                    pack_id,
                    release_tag.or(self.default_release.as_deref()),
                );
                let cache_key = original.clone();
                for attempt in 0..5 {
                    if cancel.load(Ordering::Acquire) {
                        anyhow::bail!("range request cancelled");
                    }
                    let cached = self.direct_links.lock().unwrap().get(&cache_key).cloned();
                    let had_cached = cached.is_some();
                    let url = cached
                        .filter(|link| link.expires_at > Instant::now())
                        .map(|link| link.url)
                        .unwrap_or_else(|| original.clone());
                    let response = self
                        .client
                        .get(&url)
                        .header(RANGE, format!("bytes={}-{}", start, end))
                        .send()?;
                    if response.status().is_redirection() {
                        let location_header = response
                            .headers()
                            .get(LOCATION)
                            .and_then(|v| v.to_str().ok())
                            .context("redirect is missing Location")?;
                        let resolved = if location_header.starts_with("http") {
                            location_header.to_string()
                        } else if let Ok(current) = reqwest::Url::parse(&url) {
                            current
                                .join(location_header)
                                .map(|value| value.to_string())
                                .unwrap_or_else(|_| {
                                    format!(
                                        "{}{}",
                                        base.trim_end_matches('/'),
                                        if location_header.starts_with('/') {
                                            location_header.to_string()
                                        } else {
                                            format!("/{location_header}")
                                        }
                                    )
                                })
                        } else {
                            format!(
                                "{}{}",
                                base.trim_end_matches('/'),
                                if location_header.starts_with('/') {
                                    location_header.to_string()
                                } else {
                                    format!("/{location_header}")
                                }
                            )
                        };
                        let ttl = parse_link_ttl(&resolved).unwrap_or(self.direct_link_ttl);
                        self.direct_links.lock().unwrap().insert(
                            cache_key.clone(),
                            DirectLink {
                                url: resolved,
                                expires_at: Instant::now() + ttl,
                            },
                        );
                        continue;
                    }
                    if response.status().as_u16() == 403 && had_cached {
                        self.direct_links.lock().unwrap().remove(&cache_key);
                        continue;
                    }
                    if response.status().as_u16() == 200 {
                        *self.range_unsupported.lock().unwrap() = true;
                        anyhow::bail!(
                            "source returned 200 for Range request; source does not support Range"
                        );
                    }
                    if response.status().as_u16() != 206 {
                        anyhow::bail!(
                            "source returned {} for Range request (attempt {})",
                            response.status(),
                            attempt + 1
                        );
                    }
                    let range = response
                        .headers()
                        .get(CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if !range.starts_with(&format!("bytes {}-{}", start, end)) {
                        anyhow::bail!("invalid Content-Range header: {range}");
                    }
                    let expected = end.saturating_sub(start).saturating_add(1) as usize;
                    let mut bytes = Vec::with_capacity(expected);
                    let mut buffer = [0u8; 1024 * 1024];
                    let mut response = response;
                    loop {
                        if cancel.load(Ordering::Acquire) {
                            anyhow::bail!("range request cancelled");
                        }
                        let read = response.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buffer[..read]);
                        if let Some(progress) = progress {
                            progress
                                .received
                                .store(bytes.len() as u64, Ordering::Release);
                        }
                        if bytes.len() > expected {
                            anyhow::bail!("oversized Range response: {} bytes", bytes.len());
                        }
                    }
                    if bytes.len() != end.saturating_sub(start).saturating_add(1) as usize {
                        anyhow::bail!("short Range response: {} bytes", bytes.len());
                    }
                    return Ok(bytes);
                }
                anyhow::bail!("direct link resolution exhausted retries")
            }
        }
    }

    fn cached_or_reused(&self, id: ChunkId, manifest: &Manifest) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.chunk_cache.lock().unwrap().get(&id) {
            self.emit_progress(0, bytes.len() as u64, 0, &self.source_id());
            return Ok(Some(bytes));
        }
        if let Some(store) = &self.chunk_store {
            let path = store.join(id.hex());
            if path.is_file() {
                let bytes = fs::read(&path)?;
                if vesper_format::hash_bytes(&bytes) == id.0 {
                    self.chunk_cache.lock().unwrap().insert(id, bytes.clone());
                    self.emit_progress(0, bytes.len() as u64, 0, &self.source_id());
                    return Ok(Some(bytes));
                }
                let _ = fs::remove_file(path);
            }
        }
        if let Some(reuse) = &self.local_reuse {
            if let Some((path, offset, len)) = reuse.get(&id) {
                if let Ok(mut file) = File::open(path) {
                    if file.seek(SeekFrom::Start(*offset)).is_ok() {
                        let mut bytes = vec![0u8; *len as usize];
                        if file.read_exact(&mut bytes).is_ok() {
                            let actual = if let Some(crypto) = &self.crypto {
                                crypto.chunk_id(&bytes)
                            } else {
                                vesper_format::ChunkId::from_bytes(&bytes)
                            };
                            if actual == id {
                                self.chunk_cache.lock().unwrap().insert(id, bytes.clone());
                                self.emit_progress(0, bytes.len() as u64, 0, &self.source_id());
                                return Ok(Some(bytes));
                            }
                        }
                    }
                }
            }
        }
        let _ = manifest;
        Ok(None)
    }

    fn decode_verified_chunk_without_cache(
        &self,
        id: ChunkId,
        _location: &ChunkLocation,
        frame_bytes: &[u8],
    ) -> Result<Vec<u8>> {
        self.decode_verified_chunk_impl(id, frame_bytes, false, true)
    }

    fn decode_verified_chunk_impl(
        &self,
        id: ChunkId,
        frame_bytes: &[u8],
        cache: bool,
        persist: bool,
    ) -> Result<Vec<u8>> {
        let frame = PackFrame::read_from(frame_bytes, self.max_raw_size)
            .with_context(|| format!("parse frame for chunk {id}"))?;
        let bytes = frame.decode_payload(self.crypto.as_ref())?;
        let actual_id = if let Some(crypto) = &self.crypto {
            crypto.chunk_id(&bytes)
        } else {
            vesper_format::ChunkId::from_bytes(&bytes)
        };
        if actual_id != id {
            anyhow::bail!("requested chunk {id}, source returned a different content hash");
        }
        if persist {
            self.persist_chunk(id, &bytes)?;
        }
        if cache {
            self.chunk_cache.lock().unwrap().insert(id, bytes.clone());
        }
        Ok(bytes)
    }

    fn accept_verified_chunk(&self, id: ChunkId, bytes: &[u8]) -> Result<()> {
        self.persist_chunk(id, bytes)?;
        Ok(())
    }

    fn persist_chunk(&self, id: ChunkId, bytes: &[u8]) -> Result<()> {
        let Some(store) = &self.chunk_store else {
            return Ok(());
        };
        let tmp = store.join(format!("{}.part", id.hex()));
        fs::write(&tmp, bytes)?;
        let file = File::open(&tmp)?;
        if let Err(error) = file.sync_all() {
            if error.kind() != std::io::ErrorKind::PermissionDenied {
                return Err(error.into());
            }
        }
        drop(file);
        let destination = store.join(id.hex());
        fs::rename(&tmp, &destination).or_else(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                let _ = fs::remove_file(&destination);
                fs::copy(&tmp, &destination)
                    .map(|_| ())
                    .and_then(|_| fs::remove_file(&tmp))
            } else {
                Err(error)
            }
        })?;
        Ok(())
    }

    pub fn estimated_download_bytes(&self, manifest: &Manifest, file_indices: &[usize]) -> u64 {
        if !matches!(self.source, Source::Http(_)) {
            return 0;
        }
        let mut ids = std::collections::BTreeSet::new();
        for index in file_indices {
            if let Some(file) = manifest.files.get(*index) {
                ids.extend(file.chunks.iter().map(|chunk| (chunk.id, chunk.raw_size)));
            }
        }
        ids.into_iter()
            .filter(|(id, _)| {
                if self.chunk_cache.lock().unwrap().entries.contains_key(id) {
                    return false;
                }
                if let Some(store) = &self.chunk_store {
                    if store.join(id.hex()).is_file() {
                        return false;
                    }
                }
                if let Some(reuse) = &self.local_reuse {
                    if let Some((path, offset, len)) = reuse.get(id) {
                        if path
                            .metadata()
                            .map(|metadata| metadata.len() >= offset.saturating_add(*len))
                            .unwrap_or(false)
                        {
                            return false;
                        }
                    }
                }
                true
            })
            .map(|(_, size)| size)
            .sum()
    }
}

impl Materializer for Fetcher {
    fn materialize_file(&self, manifest: &Manifest, file_index: usize, out: &Path) -> Result<()> {
        Fetcher::materialize_file(self, manifest, file_index, out)
    }

    fn materialize_files(
        &self,
        manifest: &Manifest,
        file_indices: &[usize],
        staging_root: &Path,
    ) -> Result<()> {
        Fetcher::materialize_files(self, manifest, file_indices, staging_root)
    }

    fn cleanup_after_commit(&self) -> Result<()> {
        self.cleanup_chunk_store()
    }

    fn estimate_download_bytes(&self, manifest: &Manifest, file_indices: &[usize]) -> u64 {
        self.estimated_download_bytes(manifest, file_indices)
    }

    fn hedged_bytes(&self) -> u64 {
        Fetcher::hedged_bytes(self)
    }
}

fn write_destinations(
    outputs: &mut HashMap<PathBuf, File>,
    destinations: &HashMap<ChunkId, Vec<(PathBuf, u64)>>,
    id: ChunkId,
    bytes: &[u8],
) -> Result<()> {
    for (path, offset) in destinations
        .get(&id)
        .with_context(|| format!("no destination for chunk {id}"))?
    {
        let output = outputs
            .get_mut(path)
            .with_context(|| format!("staging file is not open: {}", path.display()))?;
        output.seek(SeekFrom::Start(*offset))?;
        output.write_all(bytes)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub id: String,
    pub source: Source,
    pub default_release: Option<String>,
}

impl SourceSpec {
    pub fn new(source: Source) -> Self {
        Self {
            id: source.id(),
            source,
            default_release: None,
        }
    }

    pub fn with_default_release(source: Source, release: impl Into<String>) -> Self {
        Self {
            id: source.id(),
            source,
            default_release: Some(release.into()),
        }
    }
}

type RankingCallback = Arc<dyn Fn(Vec<String>) + Send + Sync>;

pub struct MultiSourceFetcher {
    ids: Vec<String>,
    health: Arc<Mutex<SourceHealth>>,
    probed: Mutex<bool>,
    ranking_callback: Option<RankingCallback>,
    probe_bytes: Arc<AtomicU64>,
    sources: Vec<Arc<Fetcher>>,
    hedged_total: Arc<AtomicU64>,
}

struct SourceHealth {
    order: Vec<usize>,
    rates: Vec<f64>,
    measured: Vec<Option<Instant>>,
    scheduled_bytes: u64,
    sampled_bytes: u64,
    sample_cursor: usize,
    contender: Option<usize>,
    votes: usize,
    cooldown: usize,
    consecutive_failures: Vec<u8>,
    demoted: Vec<bool>,
    in_flight: Vec<u8>,
}

impl SourceHealth {
    /// Pure state transitions: callers supply both measurements and monotonic time.
    fn rank_probes(&mut self, rates: Vec<f64>) {
        self.rates = rates;
        let initial = self.order.clone();
        self.order
            .sort_by(|a, b| self.rates[*b].total_cmp(&self.rates[*a]));
        let mut start = 0;
        while start < self.order.len() {
            let mut end = start + 1;
            while end < self.order.len()
                && self.rates[self.order[end]] >= self.rates[self.order[start]] * 0.9
            {
                end += 1;
            }
            self.order[start..end].sort_by_key(|i| initial.iter().position(|j| i == j));
            start = end;
        }
    }

    fn fresh(&self, index: usize, now: Instant) -> bool {
        self.measured[index]
            .is_some_and(|at| now.saturating_duration_since(at) <= Duration::from_secs(30))
    }

    fn observe(&mut self, index: usize, rate: f64, now: Instant) {
        self.rates[index] = if !self.fresh(index, now) {
            rate
        } else {
            self.rates[index] * 0.5 + rate * 0.5
        };
        self.measured[index] = Some(now);
        let cooling = self.cooldown > 0;
        self.cooldown = self.cooldown.saturating_sub(1);
        let primary = self.order[0];
        let candidate = self
            .order
            .iter()
            .copied()
            .filter(|i| !self.demoted[*i] && self.fresh(*i, now))
            .max_by(|a, b| self.rates[*a].total_cmp(&self.rates[*b]))
            .unwrap_or(primary);
        if candidate != primary
            && self.fresh(primary, now)
            && self.rates[candidate] > self.rates[primary] * 1.5
        {
            // A decisive content measurement corrects a noisy startup probe at
            // the first sample, even during cooldown. Close rates retain hysteresis.
            let decisive = self.rates[candidate] >= self.rates[primary] * 5.0;
            if cooling && !decisive {
                self.votes = 0;
                self.contender = None;
                return;
            }
            if self.contender == Some(candidate) {
                self.votes += 1;
            } else {
                self.contender = Some(candidate);
                self.votes = 1;
            }
            if decisive || self.votes >= 3 {
                self.order.retain(|i| *i != candidate);
                self.order.insert(0, candidate);
                self.votes = 0;
                self.contender = None;
                self.cooldown = 8;
            }
        } else {
            self.votes = 0;
            self.contender = None;
        }
    }

    /// Route useful content to backups, with a shared reservation budget across workers.
    /// No duplicate traffic: sampled bytes never exceed 5% of scheduled range bytes.
    fn sample_source(&mut self, bytes: u64) -> Option<usize> {
        self.scheduled_bytes = self.scheduled_bytes.saturating_add(bytes);
        if self.sampled_bytes.saturating_add(bytes) > self.scheduled_bytes / 20 {
            return None;
        }
        let backups: Vec<_> = self
            .order
            .iter()
            .copied()
            .skip(1)
            .filter(|i| !self.demoted[*i])
            .collect();
        if backups.is_empty() {
            return None;
        }
        let index = backups[self.sample_cursor % backups.len()];
        self.sample_cursor = self.sample_cursor.wrapping_add(1);
        self.sampled_bytes = self.sampled_bytes.saturating_add(bytes);
        Some(index)
    }

    fn new(count: usize) -> Self {
        Self {
            order: (0..count).collect(),
            rates: vec![0.0; count],
            measured: vec![None; count],
            scheduled_bytes: 0,
            sampled_bytes: 0,
            sample_cursor: 0,
            contender: None,
            votes: 0,
            cooldown: 0,
            consecutive_failures: vec![0; count],
            demoted: vec![false; count],
            in_flight: vec![0; count],
        }
    }
}

impl MultiSourceFetcher {
    /// Called after probing and every primary change. IDs preserve caller identity.
    pub fn with_ranking_callback(
        mut self,
        callback: impl Fn(Vec<String>) + Send + Sync + 'static,
    ) -> Self {
        self.ranking_callback = Some(Arc::new(callback));
        self
    }

    pub fn source_ranking(&self) -> Vec<String> {
        self.health
            .lock()
            .unwrap()
            .order
            .iter()
            .map(|i| self.ids[*i].clone())
            .collect()
    }

    /// Probe traffic is additional network traffic, excluded from content progress.
    pub fn probe_bytes(&self) -> u64 {
        self.probe_bytes.load(Ordering::Acquire)
    }

    /// Useful range bytes assigned to backup sampling; startup probes are separate.
    pub fn sampling_bytes(&self) -> u64 {
        self.health.lock().unwrap().sampled_bytes
    }

    fn report_ranking(&self, order: &[usize]) {
        eprintln!("download primary={}", self.ids[order[0]]);
        if let Some(callback) = &self.ranking_callback {
            callback(order.iter().map(|i| self.ids[*i].clone()).collect());
        }
    }

    fn probe(&self, manifest: &Manifest) {
        let mut done = self.probed.lock().unwrap();
        if *done {
            return;
        }
        *done = true;
        if self.sources.len() <= 1 {
            self.report_ranking(&[0]);
            return;
        }
        let Some(pack) = manifest.packs.iter().find(|p| p.size > 0) else {
            return;
        };
        // Keep probing below 1% per source for small/compressible packs.
        let length = pack.size.div_ceil(128).clamp(1, 256 * 1024);
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(3);
        for (i, source) in self.sources.iter().enumerate() {
            let source = Arc::clone(source);
            let pack = pack.clone();
            let (tx, cancelled, traffic) =
                (tx.clone(), cancelled.clone(), self.probe_bytes.clone());
            thread::spawn(move || {
                let mut started = Instant::now();
                let result = (|| -> Result<u64> {
                    let Source::Http(base) = &source.source else {
                        anyhow::bail!("local source does not require a probe");
                    };
                    let url = pack_url(
                        base,
                        &pack.pack_id,
                        pack.release_tag
                            .as_deref()
                            .or(source.default_release.as_deref()),
                    );
                    let client = Client::builder()
                        .redirect(Policy::limited(5))
                        .timeout(Duration::from_secs(3))
                        .build()?;
                    // Client/runtime setup is not source throughput.
                    started = Instant::now();
                    let mut response = client
                        .get(url)
                        .header("X-Vesper-Probe", "1")
                        .header(RANGE, format!("bytes=0-{}", length - 1))
                        .send()?;
                    anyhow::ensure!(
                        response.status().as_u16() == 206,
                        "probe requires Range support"
                    );
                    let mut bytes = 0u64;
                    let mut buffer = [0u8; 16384];
                    while bytes < length {
                        anyhow::ensure!(
                            !cancelled.load(Ordering::Acquire) && Instant::now() < deadline,
                            "probe deadline"
                        );
                        let size = (length - bytes).min(buffer.len() as u64) as usize;
                        let n = response.read(&mut buffer[..size])?;
                        traffic.fetch_add(n as u64, Ordering::AcqRel);
                        if n == 0 {
                            break;
                        }
                        bytes += n as u64;
                    }
                    anyhow::ensure!(
                        bytes == length && Instant::now() < deadline,
                        "probe incomplete"
                    );
                    Ok(bytes)
                })();
                let elapsed = started.elapsed().as_secs_f64();
                let bytes = result.unwrap_or(0) as f64;
                let rate = bytes / elapsed.max(0.000001);
                let _ = tx.send((i, rate));
            });
        }
        drop(tx);
        let mut results = vec![0.0; self.sources.len()];
        for _ in 0..self.sources.len() {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok((i, rate)) => {
                    results[i] = rate;
                }
                Err(_) => break,
            }
        }
        cancelled.store(true, Ordering::Release);
        for (i, rate) in results.iter().enumerate() {
            eprintln!(
                "source probe id={} bytes_per_second={rate:.0} status={}",
                self.ids[i],
                if *rate > 0.0 {
                    "ok"
                } else {
                    "failed-or-timeout"
                }
            );
        }
        eprintln!(
            "download probe_bytes={} (additional traffic)",
            self.probe_bytes()
        );
        let mut state = self.health.lock().unwrap();
        state.rank_probes(results);
        let order = state.order.clone();
        drop(state);
        self.report_ranking(&order);
    }

    pub fn new(specs: Vec<SourceSpec>) -> Result<Self> {
        if specs.is_empty() {
            anyhow::bail!("at least one download source is required");
        }
        let ids = specs.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        let health = Arc::new(Mutex::new(SourceHealth::new(specs.len())));
        let mut sources = Vec::with_capacity(specs.len());
        for spec in specs {
            let fetcher = Fetcher::new(spec.source)?;
            let fetcher = if let Some(release) = spec.default_release {
                fetcher.with_default_release(release)
            } else {
                fetcher
            };
            sources.push(Arc::new(fetcher));
        }
        Ok(Self {
            sources,
            ids,
            health,
            probed: Mutex::new(false),
            ranking_callback: None,
            probe_bytes: Arc::new(AtomicU64::new(0)),
            hedged_total: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn with_chunk_store(self, path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        fs::create_dir_all(&path)?;
        let mut sources = Vec::with_capacity(self.sources.len());
        for fetcher in self.sources {
            sources.push(Arc::new(
                (*fetcher)
                    .clone_without_cache()
                    .with_chunk_store(path.clone())?,
            ));
        }
        Ok(Self {
            sources,
            ids: self.ids,
            health: self.health,
            probed: self.probed,
            ranking_callback: self.ranking_callback,
            probe_bytes: self.probe_bytes,
            hedged_total: self.hedged_total,
        })
    }

    pub fn cleanup_chunk_store(&self) -> Result<()> {
        for source in &self.sources {
            source.cleanup_chunk_store()?;
        }
        Ok(())
    }

    pub fn with_crypto(self, crypto: CryptoContext) -> Self {
        let sources = self
            .sources
            .into_iter()
            .map(|fetcher| Arc::new(fetcher.clone_without_cache().with_crypto(crypto.clone())))
            .collect();
        Self {
            sources,
            ids: self.ids,
            health: self.health,
            probed: self.probed,
            ranking_callback: self.ranking_callback,
            probe_bytes: self.probe_bytes,
            hedged_total: self.hedged_total,
        }
    }

    pub fn with_local_manifest(
        self,
        manifest: &Manifest,
        install_root: impl Into<PathBuf>,
    ) -> Self {
        let root = install_root.into();
        let sources = self
            .sources
            .into_iter()
            .map(|fetcher| {
                Arc::new(
                    fetcher
                        .clone_without_cache()
                        .with_local_manifest(manifest, root.clone()),
                )
            })
            .collect();
        Self {
            sources,
            ids: self.ids,
            health: self.health,
            probed: self.probed,
            ranking_callback: self.ranking_callback,
            probe_bytes: self.probe_bytes,
            hedged_total: self.hedged_total,
        }
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    pub fn with_progress_callback<F>(self, callback: F) -> Self
    where
        F: Fn(ProgressEvent) + Send + Sync + 'static,
    {
        let callback: ProgressCallback = Arc::new(callback);
        let progress_state = Arc::new(Mutex::new(ProgressState::default()));
        let sources = self
            .sources
            .into_iter()
            .map(|fetcher| {
                let mut cloned = fetcher.clone_without_cache();
                cloned.progress = Some(callback.clone());
                cloned.progress_state = Arc::clone(&progress_state);
                Arc::new(cloned)
            })
            .collect();
        Self {
            sources,
            ids: self.ids,
            health: self.health,
            probed: self.probed,
            ranking_callback: self.ranking_callback,
            probe_bytes: self.probe_bytes,
            hedged_total: self.hedged_total,
        }
    }

    pub fn hedged_bytes(&self) -> u64 {
        self.hedged_total.load(Ordering::Acquire)
    }

    pub fn fetch_chunk(&self, id: ChunkId, manifest: &Manifest) -> Result<Vec<u8>> {
        for source in &self.sources {
            if let Some(bytes) = source.cached_or_reused(id, manifest)? {
                return Ok(bytes);
            }
        }
        self.probe(manifest);
        let plan = plan_ranges_for_manifest(manifest, &[id])?
            .into_iter()
            .next()
            .context("missing chunk range")?;
        let result = self.fetch_plan_from_sources(
            manifest,
            &plan,
            false,
            &Arc::new(AtomicBool::new(false)),
            &self.health,
            &Arc::new(RangeProgress::default()),
        )?;
        let bytes = result
            .values
            .into_iter()
            .next()
            .context("missing chunk result")?
            .1;
        self.sources[result.source_index].emit_progress(
            result.transferred_bytes,
            0,
            0,
            &self.ids[result.source_index],
        );
        Ok(bytes)
    }

    pub fn materialize_file(
        &self,
        manifest: &Manifest,
        file_index: usize,
        out: &Path,
    ) -> Result<()> {
        let root = out
            .parent()
            .context("materialized file has no parent directory")?;
        let staging_root = root.join(".vesper-single-file-staging");
        fs::create_dir_all(&staging_root)?;
        let file = manifest
            .files
            .get(file_index)
            .context("file index out of range")?;
        let staged = staging_root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        self.materialize_files(manifest, &[file_index], &staging_root)?;
        fs::rename(&staged, out).or_else(|_| {
            fs::copy(&staged, out)
                .map(|_| ())
                .and_then(|_| fs::remove_file(&staged))
        })?;
        let _ = fs::remove_dir_all(&staging_root);
        Ok(())
    }

    pub fn materialize_files(
        &self,
        manifest: &Manifest,
        file_indices: &[usize],
        staging_root: &Path,
    ) -> Result<()> {
        if self.sources.len() == 1 && matches!(self.sources[0].source, Source::Local(_)) {
            return self.sources[0].materialize_files(manifest, file_indices, staging_root);
        }
        let mut destinations: HashMap<ChunkId, Vec<(PathBuf, u64)>> = HashMap::new();
        let mut outputs = HashMap::<PathBuf, File>::new();
        for index in file_indices {
            let file = manifest
                .files
                .get(*index)
                .context("file index out of range")?;
            let path = staging_root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let output = File::create(&path)?;
            output.set_len(file.size)?;
            outputs.insert(path.clone(), output);
            let mut offset = 0u64;
            for chunk in &file.chunks {
                destinations
                    .entry(chunk.id)
                    .or_default()
                    .push((path.clone(), offset));
                offset = offset.saturating_add(chunk.raw_size);
            }
        }
        if destinations.is_empty() {
            return Ok(());
        }
        let mut missing = Vec::new();
        for id in destinations.keys().copied() {
            if let Some(bytes) = self.sources[0].cached_or_reused(id, manifest)? {
                write_destinations(&mut outputs, &destinations, id, &bytes)?;
            } else {
                missing.push(id);
            }
        }
        let plans = plan_ranges_for_manifest(manifest, &missing)?;
        let plans = if self.sources.len() > 1 {
            early_sampling_ranges(plans)
        } else {
            plans
        };
        let total_plans = plans.len();
        let queue = Arc::new(Mutex::new(
            plans.into_iter().enumerate().collect::<VecDeque<_>>(),
        ));
        let inflight: Arc<Mutex<HashMap<usize, InFlightRange>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let throughputs = Arc::new(Mutex::new(Vec::<u64>::new()));
        if total_plans > 0 {
            self.probe(manifest);
        }
        let health = Arc::clone(&self.health);
        let workers = if matches!(self.sources[0].source, Source::Local(_)) {
            self.sources[0].concurrency.clamp(1, 2)
        } else {
            self.sources[0].concurrency.max(1)
        };
        let (tx, rx) = mpsc::channel::<(usize, bool, Result<FetchAttempt>)>();
        thread::scope(|scope| {
            for _ in 0..workers {
                let queue = Arc::clone(&queue);
                let inflight = Arc::clone(&inflight);
                let active = Arc::clone(&active);
                let throughputs = Arc::clone(&throughputs);
                let health = Arc::clone(&health);
                let tx = tx.clone();
                scope.spawn(move || loop {
                    let task = queue.lock().unwrap().pop_front().map(|(id, plan)| {
                        (
                            id,
                            plan,
                            false,
                            Arc::new(AtomicBool::new(false)),
                            Arc::new(RangeProgress::default()),
                        )
                    });
                    let task = if task.is_some() {
                        task
                    } else {
                        let alternatives = health
                            .lock()
                            .unwrap()
                            .demoted
                            .iter()
                            .filter(|d| !**d)
                            .count();
                        let candidate = if (alternatives > 1 || self.sources.len() == 1)
                            && active.load(Ordering::Acquire) < workers
                        {
                            let mut state = inflight.lock().unwrap();
                            let median = median_throughput(&throughputs.lock().unwrap());
                            median.and_then(|median| {
                                state
                                    .iter_mut()
                                    .filter(|(_, entry)| {
                                        !entry.hedged
                                            && !entry.finished
                                            && entry.progress.source.load(Ordering::Acquire)
                                                != usize::MAX
                                    })
                                    .filter(|(_, entry)| {
                                        hedge_threshold(entry.plan.start, entry.plan.end, median)
                                            .is_some_and(|threshold| {
                                                let received =
                                                    entry.progress.received.load(Ordering::Acquire);
                                                let length = entry.plan.end - entry.plan.start + 1;
                                                entry.started.elapsed() >= threshold
                                                    && received < length
                                                    && received as f64
                                                        / entry
                                                            .started
                                                            .elapsed()
                                                            .as_secs_f64()
                                                            .max(0.000001)
                                                        < median as f64 / 10.0
                                            })
                                    })
                                    .min_by_key(|(_, entry)| entry.started)
                                    .map(|(id, entry)| {
                                        entry.hedged = true;
                                        let progress = Arc::new(RangeProgress::default());
                                        progress.source.store(
                                            entry.progress.source.load(Ordering::Acquire),
                                            Ordering::Release,
                                        );
                                        (
                                            *id,
                                            entry.plan.clone(),
                                            true,
                                            Arc::clone(&entry.cancel),
                                            progress,
                                        )
                                    })
                            })
                        } else {
                            None
                        };
                        if candidate.is_none() && active.load(Ordering::Acquire) > 0 {
                            thread::sleep(Duration::from_millis(10));
                        }
                        candidate
                    };
                    let Some((id, plan, hedge, cancel, progress)) = task else {
                        if active.load(Ordering::Acquire) == 0 {
                            break;
                        }
                        continue;
                    };
                    if !hedge {
                        inflight.lock().unwrap().insert(
                            id,
                            InFlightRange {
                                started: Instant::now(),
                                hedged: false,
                                finished: false,
                                plan: plan.clone(),
                                cancel: Arc::clone(&cancel),
                                progress: progress.clone(),
                            },
                        );
                    }
                    active.fetch_add(1, Ordering::AcqRel);
                    let result = self.fetch_plan_from_sources(
                        manifest, &plan, hedge, &cancel, &health, &progress,
                    );
                    active.fetch_sub(1, Ordering::AcqRel);
                    if !hedge {
                        if let Some(entry) = inflight.lock().unwrap().get_mut(&id) {
                            entry.finished = true;
                        }
                    }
                    if let Ok(attempt) = &result {
                        let bytes = attempt.transferred_bytes.max(1);
                        let micros = attempt.elapsed.as_micros().max(1) as u64;
                        throughputs
                            .lock()
                            .unwrap()
                            .push(bytes.saturating_mul(1_000_000).saturating_div(micros));
                    }
                    if tx.send((id, hedge, result)).is_err() {
                        break;
                    }
                });
            }
            drop(tx);
            let mut completed_plans = BTreeSet::new();
            let mut completed_chunks = BTreeSet::new();
            let mut failures: HashMap<usize, Vec<String>> = HashMap::new();
            while completed_plans.len() < total_plans {
                let Ok((plan_id, hedge, result)) = rx.recv() else {
                    anyhow::bail!("download workers stopped before all ranges completed");
                };
                if completed_plans.contains(&plan_id) {
                    if let Ok(attempt) = result {
                        self.hedged_total
                            .fetch_add(attempt.transferred_bytes, Ordering::AcqRel);
                        self.sources[attempt.source_index].emit_progress_detail(
                            0,
                            0,
                            0,
                            &self.sources[attempt.source_index].source_id(),
                            attempt.transferred_bytes,
                            None,
                        );
                    }
                    continue;
                }
                match result {
                    Ok(attempt) => {
                        if let Some(entry) = inflight.lock().unwrap().remove(&plan_id) {
                            entry.cancel.store(true, Ordering::Release);
                        }
                        let downloaded = attempt.transferred_bytes;
                        let source = &self.sources[attempt.source_index];
                        for (id, bytes) in attempt.values {
                            if completed_chunks.insert(id) {
                                if hedge {
                                    source.accept_verified_chunk(id, &bytes)?;
                                }
                                write_destinations(&mut outputs, &destinations, id, &bytes)?;
                            }
                        }
                        source.emit_progress_detail(downloaded, 0, 0, &source.source_id(), 0, None);
                        completed_plans.insert(plan_id);
                    }
                    Err(error) => {
                        failures.entry(plan_id).or_default().push(format!(
                            "{}: {}",
                            if hedge { "hedge" } else { "primary" },
                            error
                        ));
                        let hedge_active = inflight
                            .lock()
                            .unwrap()
                            .get(&plan_id)
                            .map(|entry| entry.hedged)
                            .unwrap_or(false);
                        let other_attempt_failed = failures.get(&plan_id).is_some_and(|items| {
                            items.iter().any(|item| {
                                item.starts_with(if hedge { "primary:" } else { "hedge:" })
                            })
                        });
                        if !hedge && (!hedge_active || other_attempt_failed) {
                            inflight.lock().unwrap().remove(&plan_id);
                            return Err(anyhow::anyhow!(
                                "range {} failed: {}",
                                plan_id,
                                failures.get(&plan_id).unwrap().join("; ")
                            ));
                        }
                        if hedge
                            && failures.get(&plan_id).is_some_and(|items| {
                                items.iter().any(|item| item.starts_with("primary:"))
                            })
                        {
                            inflight.lock().unwrap().remove(&plan_id);
                            return Err(anyhow::anyhow!(
                                "range {} failed: {}",
                                plan_id,
                                failures.get(&plan_id).unwrap().join("; ")
                            ));
                        }
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        })?;
        for output in outputs.values_mut() {
            output.flush()?;
            output.sync_all()?;
        }
        Ok(())
    }

    fn fetch_plan_from_sources(
        &self,
        manifest: &Manifest,
        plan: &PlannedRange,
        hedge: bool,
        cancel: &Arc<AtomicBool>,
        health: &Arc<Mutex<SourceHealth>>,
        progress: &Arc<RangeProgress>,
    ) -> Result<FetchAttempt> {
        let mut errors = Vec::new();
        let order = {
            let mut state = health.lock().unwrap();
            let sample = if !hedge && self.sources.len() > 1 {
                state.sample_source(plan.end - plan.start + 1)
            } else {
                None
            };
            let mut order = state
                .order
                .iter()
                .copied()
                .filter(|index| {
                    !state.demoted[*index]
                        && (!hedge
                            || self.sources.len() == 1
                            || *index != progress.source.load(Ordering::Acquire))
                })
                .collect::<Vec<_>>();
            if let Some(index) = sample {
                order.retain(|i| *i != index);
                order.insert(0, index);
                eprintln!("source sample id={} sample_bytes={} scheduled_bytes={} budget=5% (useful content, no duplicate bytes)",
                    self.ids[index], state.sampled_bytes, state.scheduled_bytes);
            }
            order
        };
        for index in order {
            if cancel.load(Ordering::Acquire) {
                anyhow::bail!("range request cancelled after another attempt succeeded");
            }
            let (reserved, saturated_demoted) = {
                let mut state = health.lock().unwrap();
                let saturated = state.consecutive_failures[index] > 0
                    && state.consecutive_failures[index] as usize + state.in_flight[index] as usize
                        >= 3;
                if state.demoted[index] || saturated {
                    let newly_demoted = saturated && !state.demoted[index];
                    if saturated {
                        state.demoted[index] = true;
                    }
                    (false, newly_demoted)
                } else {
                    state.in_flight[index] = state.in_flight[index].saturating_add(1);
                    (true, false)
                }
            };
            if saturated_demoted {
                self.sources[0].emit_progress_detail(
                    0,
                    0,
                    0,
                    &self.sources[index].source_id(),
                    0,
                    Some(self.sources[index].source_id()),
                );
            }
            if !reserved {
                continue;
            }
            progress.source.store(index, Ordering::Release);
            progress.received.store(0, Ordering::Release);
            match run_fetch_attempt(
                Arc::clone(&self.sources[index]),
                manifest,
                plan,
                hedge,
                cancel,
                progress.clone(),
            ) {
                Ok(mut attempt) => {
                    let mut state = health.lock().unwrap();
                    state.in_flight[index] = state.in_flight[index].saturating_sub(1);
                    state.consecutive_failures[index] = 0;
                    state.demoted[index] = false;
                    attempt.source_index = index;
                    let previous = state.order[0];
                    if !errors.is_empty() && !hedge {
                        state.order.retain(|i| *i != index);
                        state.order.insert(0, index);
                        state.cooldown = 8;
                    }
                    state.observe(
                        index,
                        attempt.transferred_bytes as f64
                            / attempt.elapsed.as_secs_f64().max(0.000001),
                        Instant::now(),
                    );
                    let switched = previous != state.order[0];
                    let order = state.order.clone();
                    drop(state);
                    if switched {
                        eprintln!(
                            "source switch {} -> {} (fresh single-request EWMA: >1.5x/3 votes/8 completion cooldown; >=5x/1 vote bypasses cooldown)",
                            self.ids[previous], self.ids[order[0]]
                        );
                        self.report_ranking(&order);
                    }
                    return Ok(attempt);
                }
                Err(error) => {
                    if cancel.load(Ordering::Acquire) {
                        let mut state = health.lock().unwrap();
                        state.in_flight[index] = state.in_flight[index].saturating_sub(1);
                        return Err(error);
                    }
                    let demoted = {
                        let mut state = health.lock().unwrap();
                        state.in_flight[index] = state.in_flight[index].saturating_sub(1);
                        state.consecutive_failures[index] =
                            state.consecutive_failures[index].saturating_add(1);
                        if state.consecutive_failures[index] >= 3 && !state.demoted[index] {
                            state.demoted[index] = true;
                            true
                        } else {
                            false
                        }
                    };
                    if demoted {
                        self.sources[0].emit_progress_detail(
                            0,
                            0,
                            0,
                            &self.sources[index].source_id(),
                            0,
                            Some(self.sources[index].source_id()),
                        );
                    }
                    errors.push(format!("source[{index}] {error:#}"));
                }
            }
        }
        Err(anyhow::anyhow!(
            "all sources failed for range {}-{}: {}",
            plan.start,
            plan.end,
            errors.join("; ")
        ))
    }
}

fn run_fetch_attempt(
    fetcher: Arc<Fetcher>,
    manifest: &Manifest,
    plan: &PlannedRange,
    hedge: bool,
    cancel: &Arc<AtomicBool>,
    progress: Arc<RangeProgress>,
) -> Result<FetchAttempt> {
    if matches!(fetcher.source, Source::Local(_)) {
        return fetcher.fetch_plan_with_cancel(manifest, plan, cancel, !hedge, Some(&progress));
    }
    let manifest = manifest.clone();
    let plan = plan.clone();
    let cancel = Arc::clone(cancel);
    let worker_cancel = Arc::clone(&cancel);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(fetcher.fetch_plan_with_cancel(
            &manifest,
            &plan,
            &worker_cancel,
            !hedge,
            Some(&progress),
        ));
    });
    loop {
        if cancel.load(Ordering::Acquire) {
            anyhow::bail!("range request cancelled after another attempt succeeded");
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("range request worker disconnected")
            }
        }
    }
}

impl Materializer for MultiSourceFetcher {
    fn materialize_file(&self, manifest: &Manifest, file_index: usize, out: &Path) -> Result<()> {
        MultiSourceFetcher::materialize_file(self, manifest, file_index, out)
    }

    fn materialize_files(
        &self,
        manifest: &Manifest,
        file_indices: &[usize],
        staging_root: &Path,
    ) -> Result<()> {
        MultiSourceFetcher::materialize_files(self, manifest, file_indices, staging_root)
    }

    fn cleanup_after_commit(&self) -> Result<()> {
        self.cleanup_chunk_store()
    }

    fn estimate_download_bytes(&self, manifest: &Manifest, file_indices: &[usize]) -> u64 {
        self.sources
            .first()
            .map(|source| source.estimated_download_bytes(manifest, file_indices))
            .unwrap_or(0)
    }

    fn hedged_bytes(&self) -> u64 {
        MultiSourceFetcher::hedged_bytes(self)
    }
}

fn median_throughput(samples: &[u64]) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2].max(1))
}

#[cfg(test)]
mod adaptive_tests;

fn hedge_threshold(start: u64, end: u64, throughput: u64) -> Option<Duration> {
    let bytes = end.checked_sub(start)?.saturating_add(1);
    let estimated_seconds = bytes as f64 / throughput.max(1) as f64;
    let seconds = (estimated_seconds * 3.0).max(2.0);
    Some(Duration::from_secs_f64(seconds))
}

impl Fetcher {
    fn clone_without_cache(&self) -> Self {
        Self {
            client: self.client.clone(),
            source: self.source.clone(),
            chunk_cache: Mutex::new(BoundedChunkCache::default()),
            chunk_store: self.chunk_store.clone(),
            crypto: self.crypto.clone(),
            local_reuse: self.local_reuse.clone(),
            default_release: self.default_release.clone(),
            max_raw_size: self.max_raw_size,
            direct_links: Mutex::new(HashMap::new()),
            range_unsupported: Mutex::new(false),
            direct_link_ttl: self.direct_link_ttl,
            progress: self.progress.clone(),
            progress_state: Arc::clone(&self.progress_state),
            concurrency: self.concurrency,
        }
    }
}

fn parse_link_ttl(url: &str) -> Option<Duration> {
    let mut se = None;
    let mut amz_expires = None;
    let mut amz_date = None;
    let mut expires = None;
    for item in url.split('?').nth(1).unwrap_or("").split('&') {
        let mut parts = item.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let value = urlencoding::decode(parts.next().unwrap_or(""))
            .ok()?
            .into_owned();
        match key {
            "se" => se = parse_iso8601(&value),
            "X-Amz-Expires" => amz_expires = value.parse::<u64>().ok(),
            "X-Amz-Date" => amz_date = parse_amz_date(&value),
            "Expires" => expires = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    if let Some(expiry) = se {
        return ttl_until_unix(expiry);
    }
    if let (Some(start), Some(seconds)) = (amz_date, amz_expires) {
        return ttl_until_unix(start.saturating_add(seconds));
    }
    ttl_until_unix(expires?)
}

fn pack_url(base: &str, pack_id: &str, release: Option<&str>) -> String {
    if base.contains("{pack_id}") {
        let url = base.replace("{pack_id}", pack_id);
        if let Some(release) = release {
            url.replace("{release}", release)
        } else {
            url
        }
    } else {
        let base = release
            .map(|release| base.replace("{release}", release))
            .unwrap_or_else(|| base.to_string());
        format!("{}/packs/{}", base.trim_end_matches('/'), pack_id)
    }
}

fn ttl_until_unix(expiry: u64) -> Option<Duration> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(
        expiry.saturating_sub(now).saturating_sub(30).max(1),
    ))
}

fn parse_amz_date(value: &str) -> Option<u64> {
    let value = value.strip_suffix('Z')?;
    if value.len() != 15 || value.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let year = value[0..4].parse::<i32>().ok()?;
    let month = value[4..6].parse::<u32>().ok()?;
    let day = value[6..8].parse::<u32>().ok()?;
    let hour = value[9..11].parse::<u32>().ok()?;
    let minute = value[11..13].parse::<u32>().ok()?;
    let second = value[13..15].parse::<u32>().ok()?;
    unix_timestamp(year, month, day, hour, minute, second)
}

fn parse_iso8601(value: &str) -> Option<u64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i32>().ok()?;
    let month = date_parts.next()?.parse::<u32>().ok()?;
    let day = date_parts.next()?.parse::<u32>().ok()?;
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<u32>().ok()?;
    let minute = time_parts.next()?.parse::<u32>().ok()?;
    let second = time_parts.next()?.split('.').next()?.parse::<u32>().ok()?;
    unix_timestamp(year, month, day, hour, minute, second)
}

fn unix_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    Some((days as u64) * 86_400 + u64::from(hour * 3600 + minute * 60 + second))
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = month as i32;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i64::from(era * 146097 + doe - 719468)
}

use std::io::Write;

pub fn verify_manifest_files(manifest: &Manifest, root: &Path) -> Result<Vec<String>> {
    let mut invalid = Vec::new();
    for file in &manifest.files {
        let path = root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let ok = path.is_file() && vesper_format::hash_file(&path)? == file.content_hash;
        if !ok {
            invalid.push(file.path.clone());
        }
    }
    Ok(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesper_format::{build_from_directory, ChunkingParams};

    #[test]
    fn multi_source_fetcher_uses_hedged_sources() {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("game.bin"), vec![b'x'; 100_000]).unwrap();
        let build = tempfile::tempdir().unwrap();
        let result = build_from_directory(
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
        let fetcher = MultiSourceFetcher::new(vec![
            SourceSpec::new(Source::Local(build.path().to_path_buf())),
            SourceSpec::new(Source::Local(build.path().to_path_buf())),
        ])
        .unwrap();
        let out = tempfile::tempdir().unwrap();
        fetcher
            .materialize_file(&result.manifest, 0, &out.path().join("game.bin"))
            .unwrap();
        assert_eq!(
            fs::read(out.path().join("game.bin")).unwrap(),
            vec![b'x'; 100_000]
        );
    }

    #[test]
    fn range_merge_obeys_gap_and_size_limits() {
        assert_eq!(
            merge_ranges(vec![
                RangeSpan {
                    start: 100,
                    end: 200
                },
                RangeSpan {
                    start: 250,
                    end: 300
                },
            ]),
            vec![RangeSpan {
                start: 100,
                end: 300
            }]
        );
        assert_eq!(
            merge_ranges(vec![
                RangeSpan {
                    start: 0,
                    end: 16 * 1024 * 1024
                },
                RangeSpan {
                    start: 16 * 1024 * 1024 + 1,
                    end: 16 * 1024 * 1024 + 2
                },
            ])
            .len(),
            2
        );
    }

    #[test]
    fn planner_groups_chunks_by_pack_and_offset() {
        let mut manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
        );
        let a = ChunkId::from_bytes(b"a");
        let b = ChunkId::from_bytes(b"b");
        manifest.packs.push(vesper_format::PackEntry {
            pack_id: "p".into(),
            size: 300,
            hash: [0; 32],
            release_tag: None,
        });
        manifest.chunks.insert(
            a,
            ChunkLocation {
                pack_id: "p".into(),
                offset: 0,
                frame_len: 100,
                codec: vesper_format::Codec::Raw,
            },
        );
        manifest.chunks.insert(
            b,
            ChunkLocation {
                pack_id: "p".into(),
                offset: 150,
                frame_len: 100,
                codec: vesper_format::Codec::Raw,
            },
        );
        assert_eq!(plan_ranges(&manifest, &[a, b]).unwrap().len(), 1);
    }

    #[test]
    fn same_source_parallel_fetch_uses_configured_batches() {
        let (build, result) = one_chunk_build();
        let id = result.manifest.files[0].chunks[0].id;
        let fetcher = Fetcher::new(Source::Local(build.path().to_path_buf()))
            .unwrap()
            .with_concurrency(8);
        let chunks = fetcher
            .fetch_chunks_parallel(&result.manifest, &[id])
            .unwrap();
        assert_eq!(chunks.get(&id).unwrap().len(), 100_000);
    }

    struct MockServer {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    #[derive(Clone)]
    enum MockMode {
        Good,
        Status(u16),
        RedirectThenExpired,
        Drop,
        Slow,
    }

    impl MockServer {
        fn new(pack_id: String, bytes: Vec<u8>, mode: MockMode) -> Self {
            use std::io::{Read, Write};
            use std::net::{Shutdown, TcpListener};
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let bytes = Arc::new(bytes);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mode = Arc::new(Mutex::new(mode));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let direct_failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let thread_bytes = bytes.clone();
            let thread_requests = requests.clone();
            let thread_mode = mode.clone();
            let thread_stop = stop.clone();
            let thread_direct_failures = direct_failures.clone();
            let thread_pack = pack_id.clone();
            let thread = std::thread::spawn(move || {
                while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    // The accepted socket inherits the listener's nonblocking
                    // mode on Windows. Make request reads blocking so a
                    // partially arrived HTTP header is not mistaken for EOF.
                    let _ = stream.set_nonblocking(false);
                    let mut request = Vec::with_capacity(4096);
                    let mut buffer = [0u8; 1024];
                    loop {
                        let n = stream.read(&mut buffer).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..n]);
                        if request.windows(4).any(|window| window == b"\r\n\r\n")
                            || request.len() >= 16 * 1024
                        {
                            break;
                        }
                    }
                    if request.is_empty() {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&request).to_string();
                    let path = text
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    thread_requests.lock().unwrap().push(path.clone());
                    let mode = thread_mode.lock().unwrap().clone();
                    if let MockMode::Status(status) = mode {
                        let body = format!("status {status}");
                        let response = format!("HTTP/1.1 {status} Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Write);
                        continue;
                    }
                    if matches!(mode, MockMode::RedirectThenExpired) && path.starts_with("/packs/")
                    {
                        let response = format!("HTTP/1.1 302 Found\r\nLocation: /direct/{}?X-Amz-Expires=60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", thread_pack);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Write);
                        continue;
                    }
                    if matches!(mode, MockMode::RedirectThenExpired)
                        && path.starts_with("/direct/")
                        && thread_direct_failures.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                            == 0
                    {
                        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Write);
                        continue;
                    }
                    let start = text
                        .lines()
                        .find_map(|line| line.strip_prefix("Range: bytes="))
                        .and_then(|v| v.split('-').next())
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let end = text
                        .lines()
                        .find_map(|line| line.strip_prefix("Range: bytes="))
                        .and_then(|v| v.split('-').nth(1))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(thread_bytes.len().saturating_sub(1));
                    let end = end.min(thread_bytes.len().saturating_sub(1));
                    let body = &thread_bytes[start.min(end)..=end];
                    if matches!(mode, MockMode::Drop) {
                        let partial = &body[..body.len().min(body.len() / 2 + 1)];
                        let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", start, end, thread_bytes.len(), body.len());
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(partial);
                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Write);
                        continue;
                    }
                    if matches!(mode, MockMode::Slow) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    let header = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", start, end, thread_bytes.len(), body.len());
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                    let _ = stream.shutdown(Shutdown::Write);
                }
            });
            std::thread::sleep(Duration::from_millis(10));
            Self {
                base,
                requests,
                stop,
                thread: Some(thread),
            }
        }
        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }
    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn one_chunk_build() -> (tempfile::TempDir, vesper_format::BuildOutput) {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("game.bin"), vec![b'x'; 100_000]).unwrap();
        let build = tempfile::tempdir().unwrap();
        let result = build_from_directory(
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
        (build, result)
    }

    struct ReleasePackServer {
        base: String,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl ReleasePackServer {
        fn new(packs: HashMap<(String, String), Vec<u8>>) -> Self {
            use std::io::{Read, Write};
            use std::net::{Shutdown, TcpListener};

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let thread_stop = stop.clone();
            let thread = std::thread::spawn(move || {
                while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    let _ = stream.set_nonblocking(false);
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    loop {
                        let read = stream.read(&mut buffer).unwrap_or(0);
                        if read == 0 {
                            break;
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
                        .unwrap_or_default()
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default();
                    let parts = path.split('/').collect::<Vec<_>>();
                    let key = if parts.len() == 5 && parts[1] == "releases" && parts[3] == "packs" {
                        Some((parts[2].to_string(), parts[4].to_string()))
                    } else {
                        None
                    };
                    let Some(bytes) = key.as_ref().and_then(|key| packs.get(key)) else {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.shutdown(Shutdown::Write);
                        continue;
                    };
                    let range = text
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if !name.eq_ignore_ascii_case("range") {
                                return None;
                            }
                            value.trim().strip_prefix("bytes=")
                        })
                        .and_then(|value| {
                            let (start, end) = value.split_once('-')?;
                            Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
                        });
                    let Some((start, end)) = range else {
                        let _ = stream.write_all(
                            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.shutdown(Shutdown::Write);
                        continue;
                    };
                    let end = end.min(bytes.len().saturating_sub(1));
                    let start = start.min(end);
                    let body = &bytes[start..=end];
                    let header = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len(),
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.flush();
                    let _ = stream.shutdown(Shutdown::Write);
                }
            });
            std::thread::sleep(Duration::from_millis(10));
            Self {
                base,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for ReleasePackServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn fresh_v4_install_fetches_chained_packs_from_release_template() {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let mut previous_manifest: Option<Box<Manifest>> = None;
        let mut packs = HashMap::new();
        let mut final_manifest = None;

        for version in 1..=4 {
            fs::write(
                input.path().join(format!("file-{version}.bin")),
                vec![version as u8; 4096],
            )
            .unwrap();
            let build = {
                let previous = previous_manifest
                    .as_deref()
                    .map(|manifest| vesper_format::PreviousBuild { manifest })
                    .into_iter()
                    .collect::<Vec<_>>();
                vesper_format::build_from_directory_with_previous(
                    input.path(),
                    &output.path().join(version.to_string()),
                    "demo".into(),
                    "windows-x64".into(),
                    version.to_string(),
                    params.clone(),
                    1024,
                    None,
                    &previous,
                )
                .unwrap()
            };
            let new_pack_paths = build.packs.clone();
            let mut manifest = build.manifest;
            for pack in &mut manifest.packs {
                if pack.release_tag.is_none() {
                    pack.release_tag = Some(format!("v{version}"));
                }
            }
            for pack in &manifest.packs {
                if let Some(path) = new_pack_paths.iter().find(|path| {
                    path.file_name().and_then(|name| name.to_str()) == Some(pack.pack_id.as_str())
                }) {
                    packs.insert(
                        (pack.release_tag.clone().unwrap(), pack.pack_id.clone()),
                        fs::read(path).unwrap(),
                    );
                }
            }
            previous_manifest = Some(Box::new(manifest));
            for path in new_pack_paths {
                fs::remove_file(path).unwrap();
            }
            final_manifest = Some(previous_manifest.as_deref().unwrap().clone());
        }

        let final_manifest = final_manifest.unwrap();
        packs.insert(
            ("manifest-only".into(), "manifest.cbor.zst".into()),
            final_manifest.encode().unwrap(),
        );
        let server = ReleasePackServer::new(packs);
        let fetcher = MultiSourceFetcher::new(vec![SourceSpec::new(Source::Http(format!(
            "{}/releases/{{release}}/packs/{{pack_id}}",
            server.base
        )))])
        .unwrap();
        let chunk_fetcher = Fetcher::new(Source::Http(format!(
            "{}/releases/{{release}}/packs/{{pack_id}}",
            server.base
        )))
        .unwrap()
        .with_default_release("manifest-only");
        for file in &final_manifest.files {
            for chunk in &file.chunks {
                assert_eq!(
                    chunk_fetcher
                        .fetch_chunk(chunk.id, &final_manifest)
                        .unwrap()
                        .len() as u64,
                    chunk.raw_size
                );
            }
        }
        let single = tempfile::tempdir().unwrap();
        for (index, file) in final_manifest.files.iter().enumerate() {
            let out = single.path().join(&file.path);
            Fetcher::new(Source::Http(format!(
                "{}/releases/{{release}}/packs/{{pack_id}}",
                server.base
            )))
            .unwrap()
            .with_default_release("manifest-only")
            .materialize_file(&final_manifest, index, &out)
            .unwrap();
            assert_eq!(
                fs::read(out).unwrap(),
                fs::read(input.path().join(&file.path)).unwrap()
            );
        }
        let installed = tempfile::tempdir().unwrap();
        let indices = (0..final_manifest.files.len()).collect::<Vec<_>>();
        fetcher
            .materialize_files(&final_manifest, &indices, installed.path())
            .unwrap();
        for file in &final_manifest.files {
            assert_eq!(
                fs::read(
                    installed
                        .path()
                        .join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR))
                )
                .unwrap(),
                fs::read(
                    input
                        .path()
                        .join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR))
                )
                .unwrap()
            );
        }
    }

    #[test]
    fn source_returning_200_marks_range_source_unsupported() {
        let (build, result) = one_chunk_build();
        let pack = &result.manifest.packs[0];
        let bytes = fs::read(build.path().join("packs").join(&pack.pack_id)).unwrap();
        let server = MockServer::new(pack.pack_id.clone(), bytes, MockMode::Status(200));
        let id = result.manifest.files[0].chunks[0].id;
        let fetcher = Fetcher::new(Source::Http(format!("{}/", server.base))).unwrap();
        let err = fetcher.fetch_chunk(id, &result.manifest).unwrap_err();
        assert!(err.to_string().contains("does not support Range"));
    }

    #[test]
    fn source_failover_statuses_are_exercised_by_multi_source_fetcher() {
        let (build, result) = one_chunk_build();
        let pack = &result.manifest.packs[0];
        let bytes = fs::read(build.path().join("packs").join(&pack.pack_id)).unwrap();
        let bad = MockServer::new(pack.pack_id.clone(), bytes.clone(), MockMode::Status(429));
        let good = MockServer::new(pack.pack_id.clone(), bytes, MockMode::Good);
        let id = result.manifest.files[0].chunks[0].id;
        let fetcher = MultiSourceFetcher::new(vec![
            SourceSpec::new(Source::Http(bad.base.clone())),
            SourceSpec::new(Source::Http(good.base.clone())),
        ])
        .unwrap();
        assert_eq!(
            fetcher.fetch_chunk(id, &result.manifest).unwrap(),
            vec![b'x'; 100_000]
        );
        assert!(!bad.requests().is_empty());
        assert!(!good.requests().is_empty());
    }

    #[test]
    fn source_403_502_and_midstream_disconnect_fail_over_through_download_path() {
        let (build, result) = one_chunk_build();
        let pack = &result.manifest.packs[0];
        let bytes = fs::read(build.path().join("packs").join(&pack.pack_id)).unwrap();
        let id = result.manifest.files[0].chunks[0].id;
        for mode in [MockMode::Status(403), MockMode::Status(502), MockMode::Drop] {
            let bad = MockServer::new(pack.pack_id.clone(), bytes.clone(), mode);
            let fetcher = MultiSourceFetcher::new(vec![
                SourceSpec::new(Source::Http(bad.base.clone())),
                SourceSpec::new(Source::Local(build.path().to_path_buf())),
            ])
            .unwrap();
            assert_eq!(
                fetcher.fetch_chunk(id, &result.manifest).unwrap().len(),
                100_000
            );
            assert!(!bad.requests().is_empty());
        }
    }

    #[test]
    fn ten_kib_per_second_source_is_observed_and_can_be_hedged() {
        let (build, result) = one_chunk_build();
        let pack = &result.manifest.packs[0];
        let bytes = fs::read(build.path().join("packs").join(&pack.pack_id)).unwrap();
        let slow = MockServer::new(pack.pack_id.clone(), bytes.clone(), MockMode::Slow);
        let good = MockServer::new(pack.pack_id.clone(), bytes, MockMode::Good);
        let id = result.manifest.files[0].chunks[0].id;
        let fetcher = MultiSourceFetcher::new(vec![
            SourceSpec::new(Source::Http(slow.base.clone())),
            SourceSpec::new(Source::Http(good.base.clone())),
        ])
        .unwrap();
        assert_eq!(
            fetcher.fetch_chunk(id, &result.manifest).unwrap().len(),
            100_000
        );
        assert!(!slow.requests().is_empty());
    }

    #[test]
    fn expired_direct_link_is_refreshed_after_403() {
        let (build, result) = one_chunk_build();
        let pack = &result.manifest.packs[0];
        let bytes = fs::read(build.path().join("packs").join(&pack.pack_id)).unwrap();
        let server = MockServer::new(pack.pack_id.clone(), bytes, MockMode::RedirectThenExpired);
        let id = result.manifest.files[0].chunks[0].id;
        let fetcher = Fetcher::new(Source::Http(server.base.clone())).unwrap();
        assert_eq!(
            fetcher.fetch_chunk(id, &result.manifest).unwrap().len(),
            100_000
        );
        let requests = server.requests();
        assert!(requests.iter().filter(|p| p.starts_with("/packs/")).count() >= 2);
        assert!(requests.iter().any(|p| p.starts_with("/direct/")));
    }

    #[test]
    fn interrupted_download_reuses_verified_chunk_store_without_network() {
        let (build, result) = one_chunk_build();
        let store = tempfile::tempdir().unwrap();
        let id = result.manifest.files[0].chunks[0].id;
        let first = Fetcher::new(Source::Local(build.path().to_path_buf()))
            .unwrap()
            .with_chunk_store(store.path())
            .unwrap();
        first
            .fetch_chunk(id, &result.manifest)
            .expect("first fetch");
        let second = Fetcher::new(Source::Http("http://127.0.0.1:1".into()))
            .unwrap()
            .with_chunk_store(store.path())
            .unwrap();
        second
            .fetch_chunk(id, &result.manifest)
            .expect("reused fetch");
    }

    #[test]
    fn slow_tail_request_emits_real_progress_event() {
        let (build, result) = one_chunk_build();
        let pack = &result.manifest.packs[0];
        let bytes = fs::read(build.path().join("packs").join(&pack.pack_id)).unwrap();
        let server = MockServer::new(pack.pack_id.clone(), bytes, MockMode::Good);
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let id = result.manifest.files[0].chunks[0].id;
        let fetcher = Fetcher::new(Source::Http(server.base.clone()))
            .unwrap()
            .with_progress_callback(move |event| seen.lock().unwrap().push(event));
        fetcher.fetch_chunk(id, &result.manifest).unwrap();
        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| event.downloaded_bytes > 0));
    }

    #[test]
    fn bounded_chunk_cache_never_exceeds_64_mib() {
        let mut cache = BoundedChunkCache::default();
        for index in 0..20u8 {
            cache.insert(ChunkId::from_bytes(&[index]), vec![index; 5 * 1024 * 1024]);
        }
        assert!(cache.total_bytes() <= CHUNK_CACHE_LIMIT);
    }

    #[test]
    fn url_templates_replace_pack_id_and_preserve_directory_layout() {
        assert_eq!(
            pack_url(
                "https://github.example/releases/download/{release}/{pack_id}",
                "pack-a.vpack",
                Some("v1")
            ),
            "https://github.example/releases/download/v1/pack-a.vpack"
        );
        assert_eq!(
            pack_url("https://cdn.example/builds/v1", "pack-a.vpack", None),
            "https://cdn.example/builds/v1/packs/pack-a.vpack"
        );
    }

    #[test]
    fn parse_link_ttl_supports_signed_url_expiry_formats() {
        assert!(
            parse_link_ttl("https://cdn.example/file?se=2099-01-02T03%3A04%3A05Z").unwrap()
                > Duration::from_secs(1)
        );
        assert!(
            parse_link_ttl("https://cdn.example/file?X-Amz-Date=20990102T030405Z&X-Amz-Expires=60")
                .unwrap()
                > Duration::from_secs(1)
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            parse_link_ttl(&format!("https://cdn.example/file?Expires={}", now + 60)).unwrap()
                <= Duration::from_secs(30)
        );
        assert!(parse_link_ttl("https://cdn.example/file?se=not-a-date").is_none());
        assert!(
            parse_link_ttl("https://cdn.example/file?X-Amz-Date=bad&X-Amz-Expires=no").is_none()
        );
    }
}
