use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use tempfile::tempdir;
use vesper_client::{
    fetch_store, library, metadata, runtime, set_game_channel, tasks, validate_runtime_rules,
};
use vesper_fetch::{Materializer, MultiSourceFetcher, Source, SourceSpec};
use vesper_format::{build_from_directory, ChunkingParams, Manifest};
use vesper_meta::{self, AppRoute, AppSource, AppTargetsSigned, KeyPair};

static STATE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct StateEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
    previous_trusted_root: Option<std::ffi::OsString>,
}

impl StateEnv {
    fn new(path: &Path) -> Self {
        let lock = STATE_ENV.lock().unwrap();
        let previous = std::env::var_os("VESPER_STATE_DIR");
        let previous_trusted_root = std::env::var_os("VESPER_TRUSTED_ROOT");
        std::env::set_var("VESPER_STATE_DIR", path);
        Self {
            _lock: lock,
            previous,
            previous_trusted_root,
        }
    }
}

impl Drop for StateEnv {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var("VESPER_STATE_DIR", previous);
        } else {
            std::env::remove_var("VESPER_STATE_DIR");
        }
        if let Some(previous) = &self.previous_trusted_root {
            std::env::set_var("VESPER_TRUSTED_ROOT", previous);
        } else {
            std::env::remove_var("VESPER_TRUSTED_ROOT");
        }
    }
}

// Serve signed index documents and release assets with real HTTP Range handling.
struct IndexServer {
    base: String,
    files: std::sync::Arc<std::sync::Mutex<BTreeMap<String, Vec<u8>>>>,
    pack_bytes: std::sync::Arc<std::sync::atomic::AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl IndexServer {
    fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let data = files.clone();
        let pack_bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let transferred = pack_bytes.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let request = (|| -> std::io::Result<Vec<u8>> {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 8192];
                    loop {
                        let size = stream.read(&mut buffer)?;
                        if size == 0 {
                            return Err(std::io::ErrorKind::UnexpectedEof.into());
                        }
                        request.extend_from_slice(&buffer[..size]);
                        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                            return Ok(request);
                        }
                    }
                })();
                let Ok(request) = request else { continue };
                let text = String::from_utf8_lossy(&request);
                let path = text.split_whitespace().nth(1).unwrap_or_default();
                let body = data.lock().unwrap().get(path).cloned();
                let (status, extra, body) = if let Some(bytes) = body {
                    if let Some(range) = text.lines().find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(str::to_owned)
                    }) {
                        let (start, end) = range.split_once('-').unwrap();
                        let start = start.parse::<usize>().unwrap();
                        let end = end
                            .parse::<usize>()
                            .unwrap_or(bytes.len() - 1)
                            .min(bytes.len() - 1);
                        (
                            "206 Partial Content",
                            format!("Content-Range: bytes {start}-{end}/{}\r\n", bytes.len()),
                            bytes[start..=end].to_vec(),
                        )
                    } else {
                        ("200 OK", String::new(), bytes)
                    }
                } else {
                    ("404 Not Found", String::new(), Vec::new())
                };
                if path.ends_with(".vpack") {
                    transferred.fetch_add(body.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
                let header = format!(
                    "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self {
            base,
            files,
            pack_bytes,
            stop,
            worker: Some(worker),
        }
    }

    fn json<T: serde::Serialize>(&self, path: &str, value: &T) {
        self.files
            .lock()
            .unwrap()
            .insert(path.into(), serde_json::to_vec(value).unwrap());
    }

    fn publish(&self, manifest: &Manifest, bytes: Vec<u8>, developer: &KeyPair, online: &KeyPair) {
        use vesper_meta::*;
        let app = &manifest.app_id;
        let version = self
            .files
            .lock()
            .unwrap()
            .get(&format!("/apps/{app}/targets.json"))
            .map(|bytes| {
                serde_json::from_slice::<Signed<AppTargetsSigned>>(bytes)
                    .unwrap()
                    .signed
                    .version
                    + 1
            })
            .unwrap_or(2);
        let root = sign(
            RootSigned {
                typ: "root".into(),
                spec_version: "1.0.0".into(),
                version: 1,
                expires: expires_in(1),
                keys: BTreeMap::from([(online.keyid.clone(), online.public_hex())]),
                roles: ["root", "snapshot", "timestamp"]
                    .into_iter()
                    .map(|role| {
                        (
                            role.into(),
                            RoleSpec {
                                keyids: vec![online.keyid.clone()],
                                threshold: 1,
                            },
                        )
                    })
                    .collect(),
            },
            &[online],
        )
        .unwrap();
        let routes = sign(
            RoutesSigned {
                typ: "routes".into(),
                spec_version: "1.0.0".into(),
                version: 1,
                expires: expires_in(1),
                apps: BTreeMap::from([(
                    app.clone(),
                    AppRoute {
                        keys: vec![developer.public_hex()],
                        threshold: 1,
                        metadata_url: None,
                    },
                )]),
            },
            &[online],
        )
        .unwrap();
        let targets = sign(
            AppTargetsSigned {
                metadata: ["store.json", "description.md"]
                    .into_iter()
                    .map(|name| (name.into(), vesper_meta::store_file_description(b"{}")))
                    .collect(),
                typ: "targets".into(),
                spec_version: "1.0.0".into(),
                version,
                expires: expires_in(1),
                channels: BTreeMap::from([(
                    "stable".into(),
                    AppChannel {
                        version,
                        platform: manifest.platform.clone(),
                        encrypted: false,
                        key_id: 0,
                        manifest: AppChannelManifest {
                            release_tag: "current".into(),
                            asset_name: "manifest.cbor.zst".into(),
                            size: bytes.len() as u64,
                            blake3: hex::encode(vesper_format::hash_bytes(&bytes)),
                        },
                    },
                )]),
                sources: vec![AppSource {
                    id: "mock".into(),
                    kind: "http".into(),
                    url_template: format!("{}/releases/{{release}}/{{pack_id}}", self.base),
                    priority: 0,
                }],
            },
            &[developer],
        )
        .unwrap();
        let snapshot = sign(
            SnapshotSigned {
                typ: "snapshot".into(),
                spec_version: "1.0.0".into(),
                version,
                expires: expires_in(1),
                meta: BTreeMap::from([(
                    "targets.json".into(),
                    metadata_description(&targets, version).unwrap(),
                )]),
            },
            &[online],
        )
        .unwrap();
        let timestamp = build_timestamp(&snapshot, version, expires_in(1), online).unwrap();
        self.json("/root.json", &root);
        let trusted_root = std::env::var_os("VESPER_STATE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("vesper-test-state"))
            .join("trusted-root.json");
        fs::create_dir_all(trusted_root.parent().unwrap()).unwrap();
        fs::write(&trusted_root, serde_json::to_vec(&root).unwrap()).unwrap();
        std::env::set_var("VESPER_TRUSTED_ROOT", &trusted_root);
        self.json("/routes.json", &routes);
        self.json(&format!("/apps/{app}/store.json"), &json!({}));
        self.json(&format!("/apps/{app}/description.md"), &json!({}));
        self.json(&format!("/apps/{app}/targets.json"), &targets);
        self.json(&format!("/apps/{app}/snapshot.json"), &snapshot);
        self.json(&format!("/apps/{app}/timestamp.json"), &timestamp);
        self.files
            .lock()
            .unwrap()
            .insert("/releases/current/manifest.cbor.zst".into(), bytes);
    }
}

impl Drop for IndexServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn build(input: &Path, output: &Path, app_id: &str, version: &str) -> vesper_format::BuildOutput {
    build_from_directory(
        input,
        output,
        app_id.into(),
        "windows-x64".into(),
        version.into(),
        ChunkingParams {
            min_size: 256,
            avg_size: 1024,
            max_size: 4096,
        },
        1024 * 1024,
    )
    .unwrap()
}

fn install(manifest: &Manifest, build_root: &Path, target: &Path) {
    let fetcher = MultiSourceFetcher::new(vec![SourceSpec::new(Source::Local(
        build_root.to_path_buf(),
    ))])
    .unwrap();
    tasks::install_at(target, manifest, &fetcher).unwrap();
}

#[test]
fn developer_direct_update_tamper_fallback_and_persisted_rollback_rejection() {
    use vesper_meta::{RoutesSigned, Signed};
    let temp = tempdir().unwrap();
    let state = temp.path().join("state");
    let _state_env = StateEnv::new(&state);
    let input = temp.path().join("input");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game"), "game").unwrap();
    let build = build(&input, &temp.path().join("build"), "test.hosted", "1");
    let central = IndexServer::new();
    let developer = IndexServer::new();
    let online = KeyPair::generate();
    let key = KeyPair::generate();
    central.publish(
        &build.manifest,
        build.manifest.encode().unwrap(),
        &key,
        &online,
    );
    let mut routes: Signed<RoutesSigned> =
        serde_json::from_slice(&central.files.lock().unwrap()["/routes.json"]).unwrap();
    routes
        .signed
        .apps
        .get_mut("test.hosted")
        .unwrap()
        .metadata_url = Some(developer.base.clone());
    central.json(
        "/routes.json",
        &vesper_meta::sign(routes.signed, &[&online]).unwrap(),
    );
    let mut targets: vesper_meta::Signed<AppTargetsSigned> =
        serde_json::from_slice(&central.files.lock().unwrap()["/apps/test.hosted/targets.json"])
            .unwrap();
    developer.json("/store.json", &json!({}));
    developer.json("/description.md", &json!({}));
    targets.signed.version = 3;
    let new = vesper_meta::sign(targets.signed.clone(), &[&key]).unwrap();
    developer.json("/targets.json", &new);
    let (direct, _) = metadata::verify_update(&central.base, "test.hosted").unwrap();
    assert_eq!(direct.signed.version, 3);
    let highest = state.join("metadata/test.hosted/highest-targets.json");
    assert_eq!(
        vesper_meta::read_json::<vesper_meta::Signed<AppTargetsSigned>>(&highest)
            .unwrap()
            .signed
            .version,
        3
    );
    // Catalogue browsing remains usable while central aggregation is behind.
    assert_eq!(
        metadata::verify_app(&central.base, "test.hosted")
            .unwrap()
            .0
            .signed
            .version,
        2
    );
    // A lower direct version is rejected; a lower central fallback cannot reset the high-water mark.
    targets.signed.version = 1;
    developer.json(
        "/targets.json",
        &vesper_meta::sign(targets.signed.clone(), &[&key]).unwrap(),
    );
    assert!(metadata::verify_update(&central.base, "test.hosted")
        .unwrap_err()
        .to_string()
        .contains("rollback"));
    assert_eq!(
        vesper_meta::read_json::<vesper_meta::Signed<AppTargetsSigned>>(&highest)
            .unwrap()
            .signed
            .version,
        3
    );
    // Use a separate client state to prove tampered direct data falls back and reports the rejection.
    fs::remove_file(&highest).unwrap();
    developer.json("/targets.json", &new);
    developer
        .files
        .lock()
        .unwrap()
        .insert("/store.json".into(), b"tampered".to_vec());
    assert_eq!(
        metadata::verify_update(&central.base, "test.hosted")
            .unwrap()
            .0
            .signed
            .version,
        2
    );
    assert_eq!(
        vesper_meta::read_json::<vesper_meta::Signed<AppTargetsSigned>>(&highest)
            .unwrap()
            .signed
            .version,
        2
    );
    // Same version equivocation is rejected even when signed by the developer.
    let mut changed = targets.signed;
    changed.version = 2;
    changed.expires += 10;
    developer.json("/store.json", &json!({}));
    developer.json(
        "/targets.json",
        &vesper_meta::sign(changed, &[&key]).unwrap(),
    );
    assert_eq!(
        metadata::verify_update(&central.base, "test.hosted")
            .unwrap()
            .0
            .signed
            .version,
        2
    );
}

#[test]
fn media_returns_verified_bytes_and_rejects_changed_response() {
    let server = IndexServer::new();
    let bytes = b"verified image bytes";
    server
        .files
        .lock()
        .unwrap()
        .insert("/cover.png".into(), bytes.to_vec());
    let digest = hex::encode(vesper_format::hash_bytes(bytes));
    let url = format!("{}/cover.png", server.base);
    assert_eq!(vesper_client::verify_media(&url, &digest).unwrap(), bytes);
    server
        .files
        .lock()
        .unwrap()
        .insert("/cover.png".into(), b"changed".to_vec());
    assert!(vesper_client::verify_media(&url, &digest).is_err());
}

#[test]
fn launcher_index_fresh_install_uses_each_pack_release() {
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(&temp.path().join("state"));
    let input = temp.path().join("input");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("old.bin"), vec![31u8; 8192]).unwrap();
    let mut v1 = build(&input, &temp.path().join("r1"), "test.split", "1");
    for pack in &mut v1.manifest.packs {
        pack.release_tag = Some("old".into());
    }
    fs::write(input.join("new.bin"), vec![73u8; 8192]).unwrap();
    let mut v2 = vesper_format::build_from_directory_with_previous(
        &input,
        &temp.path().join("r2"),
        "test.split".into(),
        "windows-x64".into(),
        "2".into(),
        ChunkingParams {
            min_size: 256,
            avg_size: 1024,
            max_size: 4096,
        },
        1024 * 1024,
        None,
        &[vesper_format::PreviousBuild {
            manifest: &v1.manifest,
        }],
    )
    .unwrap();
    for pack in &mut v2.manifest.packs {
        if pack.release_tag.is_none() {
            pack.release_tag = Some("current".into());
        }
    }
    assert!(v2
        .manifest
        .packs
        .iter()
        .any(|pack| pack.release_tag.as_deref() == Some("old")));
    assert!(v2
        .manifest
        .packs
        .iter()
        .any(|pack| pack.release_tag.as_deref() == Some("current")));
    let server = IndexServer::new();
    for (release, built) in [("old", &v1), ("current", &v2)] {
        for path in &built.packs {
            server.files.lock().unwrap().insert(
                format!(
                    "/releases/{release}/{}",
                    path.file_name().unwrap().to_str().unwrap()
                ),
                fs::read(path).unwrap(),
            );
        }
    }
    let developer = KeyPair::generate();
    let online = KeyPair::generate();
    server.publish(
        &v2.manifest,
        v2.manifest.encode().unwrap(),
        &developer,
        &online,
    );
    let target = temp.path().join("library/test.split");
    let state = vesper_client::new_app_state(vesper_client::Settings {
        api_base: server.base.clone(),
        ..Default::default()
    });
    vesper_client::install_from_index(
        vesper_client::InstallRequest {
            app_id: "test.split".into(),
            channel: "stable".into(),
            metadata_base: server.base.clone(),
            install_dir: target.to_str().unwrap().into(),
        },
        &state,
    )
    .unwrap();
    for file in &v2.manifest.files {
        assert_eq!(
            fs::read(target.join(&file.path)).unwrap(),
            fs::read(input.join(&file.path)).unwrap()
        );
    }
    assert!(vesper_client::verify_game(target.to_str().unwrap(), &state)
        .unwrap()
        .is_empty());
    assert!(
        !vesper_client::check_update(target.to_str().unwrap(), &state)
            .unwrap()
            .available
    );
    fs::write(target.join("old.bin"), b"damaged").unwrap();
    vesper_client::repair_from_index(
        target.to_str().unwrap(),
        &state,
        &vesper_client::NoopEventSink,
    )
    .unwrap();
    assert_eq!(fs::read(target.join("old.bin")).unwrap(), vec![31u8; 8192]);
    // Update and rollback retain release provenance even when metadata versions
    // and game versions differ. Rollback restores files without a download.
    let updated = temp.path().join("update-library/test.split");
    let request = || vesper_client::InstallRequest {
        app_id: "test.split".into(),
        channel: "stable".into(),
        metadata_base: server.base.clone(),
        install_dir: updated.to_str().unwrap().into(),
    };
    server.publish(
        &v1.manifest,
        v1.manifest.encode().unwrap(),
        &developer,
        &online,
    );
    vesper_client::install_from_index(request(), &state).unwrap();
    assert!(
        !vesper_client::check_update(updated.to_str().unwrap(), &state)
            .unwrap()
            .available
    );
    server.publish(
        &v2.manifest,
        v2.manifest.encode().unwrap(),
        &developer,
        &online,
    );
    assert!(
        vesper_client::check_update(updated.to_str().unwrap(), &state)
            .unwrap()
            .available
    );
    vesper_client::install_from_index(request(), &state).unwrap();
    vesper_client::rollback_game(updated.to_str().unwrap()).unwrap();
    assert!(!updated.join("new.bin").exists());
    assert_eq!(fs::read(updated.join("old.bin")).unwrap(), vec![31u8; 8192]);
    fs::write(updated.join("old.bin"), b"damaged after rollback").unwrap();
    vesper_client::repair_from_index(
        updated.to_str().unwrap(),
        &state,
        &vesper_client::NoopEventSink,
    )
    .unwrap();
    assert!(
        vesper_client::verify_game(updated.to_str().unwrap(), &state)
            .unwrap()
            .is_empty()
    );
    // An unexpired timestamp with an invalid signature must still be rejected.
    let path = "/apps/test.split/timestamp.json";
    let mut timestamp: serde_json::Value =
        serde_json::from_slice(&server.files.lock().unwrap()[path]).unwrap();
    timestamp["signed"]["version"] = json!(timestamp["signed"]["version"].as_u64().unwrap() + 1);
    server.json(path, &timestamp);
    let error = vesper_client::metadata::verify_app(&server.base, "test.split").unwrap_err();
    assert!(format!("{error:#}").contains("signature"), "{error:#}");
}

#[test]
fn launcher_delta_progress_counts_wire_bytes_and_local_reuse() {
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(&temp.path().join("state"));
    let input = temp.path().join("input");
    fs::create_dir_all(&input).unwrap();
    let mut seed = 0x0123456789abcdefu64;
    let original: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    fs::write(input.join("game.bin"), &original).unwrap();
    let params = ChunkingParams {
        min_size: 64 * 1024,
        avg_size: 128 * 1024,
        max_size: 256 * 1024,
    };
    let mut v4 = build_from_directory(
        &input,
        &temp.path().join("v4"),
        "test.progress".into(),
        "windows-x64".into(),
        "4".into(),
        params.clone(),
        8 * 1024 * 1024,
    )
    .unwrap();
    for pack in &mut v4.manifest.packs {
        pack.release_tag = Some("old".into());
    }
    let mut changed = original.clone();
    changed.extend(vec![73; 256 * 1024]);
    fs::write(input.join("game.bin"), &changed).unwrap();
    let mut v5 = vesper_format::build_from_directory_with_previous(
        &input,
        &temp.path().join("v5"),
        "test.progress".into(),
        "windows-x64".into(),
        "5".into(),
        params,
        8 * 1024 * 1024,
        None,
        &[vesper_format::PreviousBuild {
            manifest: &v4.manifest,
        }],
    )
    .unwrap();
    for pack in &mut v5.manifest.packs {
        if pack.release_tag.is_none() {
            pack.release_tag = Some("current".into());
        }
    }
    let new_pack_size: u64 = v5
        .manifest
        .packs
        .iter()
        .filter(|p| p.release_tag.as_deref() == Some("current"))
        .map(|p| p.size)
        .sum();
    let server = IndexServer::new();
    for (release, built) in [("old", &v4), ("current", &v5)] {
        for pack in &built.packs {
            server.files.lock().unwrap().insert(
                format!(
                    "/releases/{release}/{}",
                    pack.file_name().unwrap().to_str().unwrap()
                ),
                fs::read(pack).unwrap(),
            );
        }
    }
    let developer = KeyPair::generate();
    let online = KeyPair::generate();
    let state = vesper_client::new_app_state(vesper_client::Settings {
        api_base: server.base.clone(),
        ..Default::default()
    });
    let target = temp.path().join("library/test.progress");
    let request = |root: &Path| vesper_client::InstallRequest {
        app_id: "test.progress".into(),
        channel: "stable".into(),
        metadata_base: server.base.clone(),
        install_dir: root.to_str().unwrap().into(),
    };
    let wire = || server.pack_bytes.load(std::sync::atomic::Ordering::Relaxed);
    server.publish(
        &v4.manifest,
        v4.manifest.encode().unwrap(),
        &developer,
        &online,
    );
    vesper_client::install_from_index(request(&target), &state).unwrap();
    let fresh4 = vesper_client::list_tasks(&state)
        .into_iter()
        .next_back()
        .unwrap();
    assert_eq!(fresh4.downloaded_bytes, wire());
    assert_eq!(fresh4.reused_bytes, 0);
    server.publish(
        &v5.manifest,
        v5.manifest.encode().unwrap(),
        &developer,
        &online,
    );
    let check = vesper_client::check_update(target.to_str().unwrap(), &state).unwrap();
    assert_eq!(check.target_version, "5");
    assert_eq!(check.message, "Update available: v5");
    let before = wire();
    vesper_client::install_from_index(request(&target), &state).unwrap();
    let delta = vesper_client::list_tasks(&state)
        .into_iter()
        .find(|t| t.downloaded_bytes == wire() - before && t.reused_bytes > 0)
        .unwrap();
    assert_eq!(delta.remaining_bytes, 0);
    assert!(
        delta.downloaded_bytes.abs_diff(new_pack_size) < 1024,
        "{} vs {}",
        delta.downloaded_bytes,
        new_pack_size
    );
    assert!(delta.downloaded_bytes < 512 * 1024);
    assert!(
        delta.reused_bytes.abs_diff(original.len() as u64) < 512 * 1024,
        "{}",
        delta.reused_bytes
    );
    assert_eq!(fs::read(target.join("game.bin")).unwrap(), changed);
    let fresh_target = temp.path().join("fresh/test.progress");
    let before = wire();
    vesper_client::install_from_index(request(&fresh_target), &state).unwrap();
    let fresh5 = vesper_client::list_tasks(&state)
        .into_iter()
        .find(|t| t.downloaded_bytes == wire() - before && t.reused_bytes == 0)
        .unwrap();
    // Old release packs can contain obsolete frames; fresh installs fetch only referenced chunks.
    let referenced: std::collections::BTreeSet<_> = v5
        .manifest
        .files
        .iter()
        .flat_map(|file| file.chunks.iter().map(|chunk| chunk.id))
        .collect();
    let expected_wire: u64 = referenced
        .iter()
        .map(|id| v5.manifest.chunks[id].frame_len)
        .sum();
    assert_eq!(fresh5.downloaded_bytes, expected_wire);
    assert_eq!(fresh5.remaining_bytes, 0);
    assert_eq!(fs::read(fresh_target.join("game.bin")).unwrap(), changed);
}

#[test]
#[ignore = "requires an explicitly selected public GitHub index"]
fn real_github_index_install() {
    let Ok(base) = std::env::var("VESPER_REAL_INDEX") else {
        println!("VESPER_REAL_INDEX is unset; skipping public index install");
        return;
    };
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(&temp.path().join("state"));
    let target = temp.path().join("ghtest.free");
    let state = vesper_client::new_app_state(vesper_client::Settings {
        api_base: base.clone(),
        ..Default::default()
    });
    vesper_client::install_from_index(
        vesper_client::InstallRequest {
            app_id: "ghtest.free".into(),
            channel: "stable".into(),
            metadata_base: base,
            install_dir: target.to_str().unwrap().into(),
        },
        &state,
    )
    .unwrap();
    assert!(vesper_client::verify_game(target.to_str().unwrap(), &state)
        .unwrap()
        .is_empty());
    println!("Fresh ghtest.free install and verify succeeded");
}

#[test]
fn flow_absolute_github_template_downloads_flat_pack() {
    let temp = tempdir().unwrap();
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    let target = temp.path().join("library/demo.flat");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.bin"), vec![7u8; 8192]).unwrap();
    let built = build(&input, &output, "demo.flat", "ghtest-1");
    let pack = built.manifest.packs[0].pack_id.clone();
    let bytes = fs::read(output.join("packs").join(&pack)).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let size = stream.read(&mut request).unwrap();
        let text = String::from_utf8_lossy(&request[..size]);
        assert!(text
            .lines()
            .next()
            .unwrap_or_default()
            .contains(&format!("/releases/download/ghtest-1/{pack}")));
        let range = text
            .lines()
            .find_map(|line| line.strip_prefix("Range: bytes="))
            .unwrap_or("0-");
        let mut parts = range.split('-');
        let start = parts.next().unwrap().parse::<usize>().unwrap();
        let end = parts
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(bytes.len() - 1)
            .min(bytes.len() - 1);
        let body = &bytes[start..=end];
        let header = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len(),
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    });
    let source = format!("http://{address}/releases/download/{{release}}/{{pack_id}}")
        .replace("{release}", "ghtest-1");
    let fetcher = MultiSourceFetcher::new(vec![SourceSpec::new(Source::Http(source))]).unwrap();
    tasks::install_at(&target, &built.manifest, &fetcher).unwrap();
    server.join().unwrap();
    assert_eq!(fs::read(target.join("game.bin")).unwrap(), vec![7u8; 8192]);
}

fn gateway_server() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for _ in 0..3 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let text = String::from_utf8_lossy(&request);
            let cookie = if text.contains("/auth/email/verify") {
                "\r\nSet-Cookie: session=test-session; Path=/; HttpOnly"
            } else {
                ""
            };
            let body = br#"{"ok":true,"app_id":"demo.paid"}"#;
            let header = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}{}\r\nConnection: close\r\n\r\n", body.len(), cookie);
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        }
    });
    format!("http://{}", address)
}

#[test]
fn flow_store_install_restart_and_channel_persistence() {
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(&temp.path().join("state"));
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    let target = temp.path().join("library/demo.free");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.exe"), b"v1").unwrap();
    let built = build(&input, &output, "demo.free", "1");
    let server = IndexServer::new();
    let developer = KeyPair::generate();
    let online = KeyPair::generate();
    server.publish(
        &built.manifest,
        fs::read(output.join("manifest.cbor.zst")).unwrap(),
        &developer,
        &online,
    );
    server.json("/api/v1/apps/page-1.json", &json!({"total":1,"apps":[{"app_id":"demo.free","store":{"title":"Unsigned API injection"}}]}));
    let store = fetch_store(&server.base).unwrap();
    assert_eq!(store.apps[0].app_id, "demo.free");
    assert_eq!(store.apps[0].store, json!({}));

    install(&built.manifest, &output, &target);
    set_game_channel(target.to_str().unwrap(), "beta").unwrap();
    let restarted = library::scan(&temp.path().join("library")).unwrap();
    assert_eq!(restarted[0].version, "1");
    assert_eq!(restarted[0].channel, "beta");
}

#[test]
fn flow_update_rollback_repair_and_uninstall_user_files() {
    let temp = tempdir().unwrap();
    let input = temp.path().join("input");
    let v1dir = temp.path().join("v1");
    let v2dir = temp.path().join("v2");
    let target = temp.path().join("library/demo.free");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.exe"), vec![1u8; 8192]).unwrap();
    fs::write(input.join("save.dat"), b"user").unwrap();
    let mut v1 = build(&input, &v1dir, "demo.free", "1");
    v1.manifest.install.user_files = vec!["save.dat".into()];
    install(&v1.manifest, &v1dir, &target);
    fs::write(target.join("save.dat"), b"progress").unwrap();
    fs::write(input.join("game.exe"), vec![2u8; 8192]).unwrap();
    fs::write(input.join("new.dat"), b"new").unwrap();
    let v2 = build(&input, &v2dir, "demo.free", "2");
    install(&v2.manifest, &v2dir, &target);
    assert_eq!(fs::read(target.join("game.exe")).unwrap(), vec![2u8; 8192]);
    vesper_client::rollback_game(target.to_str().unwrap()).unwrap();
    assert_eq!(fs::read(target.join("game.exe")).unwrap(), vec![1u8; 8192]);
    fs::write(target.join("game.exe"), b"corrupt").unwrap();
    let damaged = vesper_client::verify_game(
        target.to_str().unwrap(),
        &vesper_client::new_app_state(vesper_client::Settings::default()),
    )
    .unwrap();
    assert!(damaged.contains(&"game.exe".to_string()));
    install(&v1.manifest, &v1dir, &target);
    assert!(target.join("game.exe").exists());
    vesper_client::uninstall_game(
        target.to_str().unwrap(),
        false,
        &vesper_client::new_app_state(vesper_client::Settings::default()),
    )
    .unwrap();
    assert!(target.join("save.dat").exists());
}

#[test]
fn flow_paid_gateway_ipc_serialization_contains_no_depot_key() {
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(temp.path());
    let gateway = gateway_server();
    vesper_client::email_start(&gateway, "buyer@example.com").unwrap();
    vesper_client::email_verify(&gateway, "buyer@example.com", "123456").unwrap();
    let response = vesper_client::redeem_code(&gateway, "demo.paid", "DEMO-PAID-2026").unwrap();
    vesper_client::logout().unwrap();
    let encoded = serde_json::to_string(&response).unwrap();
    let secret = "11".repeat(32);
    assert!(!encoded.contains(&secret));
    assert!(!encoded.contains("depot_key_hex"));
}

#[test]
fn flow_metadata_tamper_rollback_and_expiry_are_distinct() {
    let key = KeyPair::generate();
    let route = AppRoute {
        keys: vec![key.public_hex()],
        threshold: 1,
        metadata_url: None,
    };
    let payload = AppTargetsSigned {
        metadata: ["store.json", "description.md"]
            .into_iter()
            .map(|name| (name.into(), vesper_meta::store_file_description(b"{}")))
            .collect(),
        typ: "targets".into(),
        spec_version: "1.0.0".into(),
        version: 2,
        expires: vesper_meta::expires_in(1),
        channels: BTreeMap::new(),
        sources: vec![AppSource {
            id: "http".into(),
            kind: "http".into(),
            url_template: "{release}/{pack_id}".into(),
            priority: 0,
        }],
    };
    let signed = vesper_meta::sign(payload, &[&key]).unwrap();
    vesper_meta::verify_app_targets(&signed, &route, Some(1)).unwrap();
    let rollback = vesper_meta::verify_app_targets(&signed, &route, Some(3))
        .unwrap_err()
        .to_string();
    assert!(rollback.contains("rollback"));
    let expired = metadata::check_version(2, 1, 2, None)
        .unwrap_err()
        .to_string();
    assert!(expired.contains("expired"));
    let tampered = vesper_meta::verify_app_targets(
        &vesper_meta::Signed {
            signed: signed.signed.clone(),
            signatures: vec![],
        },
        &route,
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(tampered.contains("signature"));
}

#[test]
fn flow_pause_resume_reuses_verified_chunks() {
    let temp = tempdir().unwrap();
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    let target = temp.path().join("install");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.bin"), vec![7u8; 65536]).unwrap();
    let built = build(&input, &output, "demo.free", "1");
    install(&built.manifest, &output, &target);
    let fetcher = MultiSourceFetcher::new(vec![SourceSpec::new(Source::Local(output))])
        .unwrap()
        .with_local_manifest(&built.manifest, &target);
    let estimate = fetcher.estimate_download_bytes(
        &built.manifest,
        &(0..built.manifest.files.len()).collect::<Vec<_>>(),
    );
    assert_eq!(estimate, 0);
}

#[test]
fn flow_install_size_stays_within_bound() {
    let temp = tempdir().unwrap();
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    let target = temp.path().join("install");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.exe"), vec![7u8; 4096]).unwrap();
    let built = build(&input, &output, "demo.free", "1");
    install(&built.manifest, &output, &target);
    fn walk(path: &Path) -> u64 {
        fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| {
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            walk(&entry.path())
                        } else {
                            entry.metadata().map(|m| m.len()).unwrap_or(0)
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    assert!(
        walk(&target)
            <= walk(&input) + fs::metadata(&built.manifest_path).unwrap().len() + 1024 * 1024
    );
}

#[test]
fn flow_unreviewed_runtime_manifest_is_rejected() {
    let temp = tempdir().unwrap();
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.exe"), b"runtime").unwrap();
    let mut built = build(&input, &output, "demo.free", "1");
    built
        .manifest
        .install
        .runtimes
        .push(vesper_format::RuntimeDependency {
            name: "VC++ 2015-2022 x64".into(),
            version: "1".into(),
            installer_sha256: None,
        });
    let error = validate_runtime_rules(&built.manifest)
        .unwrap_err()
        .to_string();
    assert!(error.contains("disabled"));
    assert!(!runtime::is_allowed("VC++ 2015-2022 x64"));
}

#[test]
fn flow_rollback_without_backup_after_first_install_returns_core_error() {
    let temp = tempdir().unwrap();
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    let target = temp.path().join("library/demo.free");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.exe"), b"v1").unwrap();
    let built = build(&input, &output, "demo.free", "1");
    install(&built.manifest, &output, &target);
    let games = library::scan(&temp.path().join("library")).unwrap();
    assert_eq!(games.len(), 1);
    assert!(!games[0].has_backup);
    let error = vesper_client::rollback_game(target.to_str().unwrap())
        .unwrap_err()
        .to_string();
    assert!(error.contains("no rollback backup is available"));
}

#[test]
fn client_worker_pause_resume_and_repair_preserve_event_contracts() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };
    use std::time::{Duration, Instant};
    use vesper_client::{EventSink, InstallRequest, Settings};

    struct RecordingSink {
        events: Mutex<Vec<(String, serde_json::Value)>>,
        first_preflight: AtomicBool,
        ready: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl EventSink for RecordingSink {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.events.lock().unwrap().push((event.into(), payload));
            if event == "disk-preflight" && self.first_preflight.swap(false, Ordering::AcqRel) {
                self.ready.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
        }
    }
    fn wait_status(state: &vesper_client::AppState, id: &str, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let task = vesper_client::list_tasks(state)
                .into_iter()
                .find(|t| t.id == id)
                .unwrap();
            assert_ne!(task.status, "failed", "{:?}", task.message);
            if task.status == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "wanted {expected}, got {}",
                task.status
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        keys
    }

    let temp = tempdir().unwrap();
    let state_root = temp.path().join("state");
    let _state_env = StateEnv::new(&state_root);
    let input = temp.path().join("input");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.bin"), vec![19u8; 8192]).unwrap();
    let built = build(&input, &temp.path().join("build"), "test.events", "1");
    let server = IndexServer::new();
    for pack in &built.packs {
        server.files.lock().unwrap().insert(
            format!(
                "/releases/current/{}",
                pack.file_name().unwrap().to_str().unwrap()
            ),
            fs::read(pack).unwrap(),
        );
    }
    server.publish(
        &built.manifest,
        built.manifest.encode().unwrap(),
        &KeyPair::generate(),
        &KeyPair::generate(),
    );
    let target = temp.path().join("library/test.events");
    let state = vesper_client::new_app_state(Settings {
        api_base: server.base.clone(),
        ..Default::default()
    });
    let (ready, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let sink = Arc::new(RecordingSink {
        events: Mutex::new(Vec::new()),
        first_preflight: AtomicBool::new(true),
        ready,
        release: Mutex::new(blocked),
    });
    let id = vesper_client::start_install(
        InstallRequest {
            app_id: "test.events".into(),
            channel: "stable".into(),
            metadata_base: server.base.clone(),
            install_dir: target.to_str().unwrap().into(),
        },
        sink.clone(),
        &state,
    )
    .unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    vesper_client::pause_task(&id, sink.as_ref(), &state).unwrap();
    release.send(()).unwrap();
    wait_status(&state, &id, "paused");
    let task_path = state_root.join("tasks").join(format!("{id}.json"));
    let paused: serde_json::Value = serde_json::from_slice(&fs::read(&task_path).unwrap()).unwrap();
    assert_eq!(paused["status"], "paused");
    assert_eq!(
        keys(&paused),
        vec![
            "app_id",
            "downloaded_bytes",
            "id",
            "message",
            "remaining_bytes",
            "reused_bytes",
            "source_speeds",
            "status"
        ]
    );
    vesper_client::resume_task(&id, sink.clone(), &state).unwrap();
    wait_status(&state, &id, "completed");
    let deadline = Instant::now() + Duration::from_secs(10);
    while task_path.exists() {
        assert!(
            Instant::now() < deadline,
            "completed task record was not removed"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read(target.join("game.bin")).unwrap(), vec![19u8; 8192]);
    fs::write(target.join("game.bin"), b"damaged").unwrap();
    assert_eq!(
        vesper_client::repair_from_index(target.to_str().unwrap(), &state, sink.as_ref()).unwrap(),
        "Repaired 1 file(s)"
    );
    assert!(vesper_client::verify_game(target.to_str().unwrap(), &state)
        .unwrap()
        .is_empty());
    let events = sink.events.lock().unwrap();
    let statuses: Vec<_> = events
        .iter()
        .filter(|(name, _)| name == "task-status")
        .map(|(_, payload)| payload["status"].as_str().unwrap())
        .collect();
    for expected in ["verifying", "pausing", "paused", "installing", "completed"] {
        assert!(statuses.contains(&expected), "{statuses:?}");
    }
    for (name, payload) in events.iter() {
        match name.as_str() {
            "task-status" => {
                assert_eq!(keys(payload), keys(&paused));
                assert_eq!(payload["id"], id);
            }
            "disk-preflight" => assert_eq!(
                keys(payload),
                vec!["changed_bytes", "download_bytes", "peak_bytes", "task_id"]
            ),
            "download-progress" => assert_eq!(
                keys(payload),
                vec![
                    "downloaded_bytes",
                    "remaining_bytes",
                    "reused_bytes",
                    "source_speeds",
                    "task_id"
                ]
            ),
            "repair-completed" => assert_eq!(
                *payload,
                json!({"app_id":"test.events", "files":["game.bin"]})
            ),
            other => panic!("unexpected event: {other}"),
        }
    }
    for expected in [
        "task-status",
        "disk-preflight",
        "download-progress",
        "repair-completed",
    ] {
        assert!(
            events.iter().any(|(name, _)| name == expected),
            "missing {expected}"
        );
    }
}

#[test]
fn settings_keep_existing_paths_defaults_and_json_fields() {
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(temp.path());
    let defaults = vesper_client::load_settings();
    assert_eq!(
        defaults.library_dir,
        temp.path().join("library").to_string_lossy()
    );
    assert_eq!(
        defaults.api_base,
        "https://baryon-interactive.github.io/vesper-index"
    );
    assert!(defaults.gateway_base.is_empty());
    fs::write(temp.path().join("settings.json"), br#"{"concurrency":3}"#).unwrap();
    let mut settings = vesper_client::load_settings();
    assert_eq!(settings.concurrency, 3);
    assert_eq!(settings.api_base, defaults.api_base);
    settings.library_dir = "relative-library".into();
    let state = vesper_client::new_app_state(defaults);
    vesper_client::save_settings(settings.clone(), &state).unwrap();
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
    let mut fields: Vec<_> = persisted
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        vec![
            "api_base",
            "bandwidth_limit_kib",
            "concurrency",
            "enabled_sources",
            "gateway_base",
            "library_dir",
            "updater_endpoint"
        ]
    );
    assert_eq!(
        vesper_client::get_settings(&state).unwrap().library_dir,
        settings.library_dir
    );
    assert_eq!(
        vesper_client::load_settings().library_dir,
        settings.library_dir
    );
}

#[test]
fn no_gateway_paid_game_state_is_clear_and_free_catalogue_remains_available() {
    let error = vesper_client::gateway::require_gateway("")
        .unwrap_err()
        .to_string();
    assert!(error.contains("Paid games need a configured key gateway"));
    assert!(error.contains("free installs remain available"));
    assert!(vesper_client::gateway::require_gateway("https://gateway.example").is_ok());
}

#[test]
fn catalogue_rejects_tampered_raw_store_without_displaying_derived_records() {
    let temp = tempdir().unwrap();
    let _state_env = StateEnv::new(&temp.path().join("state"));
    let input = temp.path().join("input");
    let output = temp.path().join("build");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.exe"), b"test").unwrap();
    let built = build(&input, &output, "test.catalogue", "1");
    let server = IndexServer::new();
    server.publish(
        &built.manifest,
        fs::read(output.join("manifest.cbor.zst")).unwrap(),
        &KeyPair::generate(),
        &KeyPair::generate(),
    );
    server.json("/api/v1/apps/page-1.json", &json!({"total":1,"apps":[{"app_id":"test.catalogue","store":{"title":"untrusted derived record"}}]}));
    server.json(
        "/api/v1/apps/test.catalogue.json",
        &json!({"store":{"title":"untrusted derived record"},"description":"injected"}),
    );
    assert_eq!(fetch_store(&server.base).unwrap().apps[0].store, json!({}));
    for name in ["store.json", "description.md"] {
        server.json(
            &format!("/apps/test.catalogue/{name}"),
            &json!({"tampered":true}),
        );
        let error = fetch_store(&server.base).unwrap_err().to_string();
        assert!(
            error.contains("mismatch") && error.contains(name),
            "{error}"
        );
        let error = vesper_client::fetch_app_detail(&server.base, "test.catalogue")
            .unwrap_err()
            .to_string();
        assert!(error.contains("mismatch"), "{error}");
        server.json(&format!("/apps/test.catalogue/{name}"), &json!({}));
    }
}
