use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

#[derive(Default)]
struct Remote {
    mirror: BTreeMap<String, Vec<u8>>,
    mirror_hashes: BTreeMap<String, String>,
    disconnect_metadata_once: bool,
    disconnect_api_once: bool,
    reject_metadata: bool,
    reject_mirror: bool,
    created: bool,
    metadata_created: bool,
    metadata_assets: BTreeMap<String, Vec<u8>>,
    assets: BTreeMap<String, Vec<u8>>,
    old_assets: BTreeMap<String, Vec<u8>>,
    requests: Vec<(String, String, Vec<u8>)>,
}

struct GitHubMock {
    base: String,
    remote: Arc<Mutex<Remote>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl GitHubMock {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let remote = Arc::new(Mutex::new(Remote::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let data = remote.clone();
        let stopping = stop.clone();
        let origin = base.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 8192];
                let header_end = loop {
                    let size = stream.read(&mut buf).unwrap();
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buf[..size]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let header = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                let mut words = header.split_whitespace();
                let method = words.next().unwrap();
                let path = words.next().unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|s| s.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let size = stream.read(&mut buf).unwrap();
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buf[..size]);
                }
                let body = bytes[header_end..header_end + length].to_vec();
                let mut remote = data.lock().unwrap();
                remote
                    .requests
                    .push((method.into(), path.into(), body.clone()));
                if method == "GET" && remote.disconnect_api_once {
                    remote.disconnect_api_once = false;
                    continue;
                }
                let release = json!({"id":2,"tag_name":"r2","upload_url":format!("{origin}/uploads{{?name,label}}"),"assets_url":format!("{origin}/assets")});
                let mut status = "200 OK";
                let mut extra = String::new();
                let metadata_release = json!({"id":3,"tag_name":"vesper-metadata","upload_url":format!("{origin}/metadata-uploads{{?name,label}}"),"assets_url":format!("{origin}/metadata-assets")});
                let names = ["targets.json", "store.json", "description.md"];
                let result = if path.starts_with("/mirror/") {
                    if remote.reject_mirror {
                        status = "503 Unavailable";
                        Vec::new()
                    } else if method == "PUT" {
                        let hash = header
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("x-amz-meta-vesper-blake3")
                                    .then(|| value.trim().to_string())
                            })
                            .unwrap();
                        remote.mirror_hashes.insert(path.into(), hash);
                        remote.mirror.insert(path.into(), body);
                        Vec::new()
                    } else if let Some(value) = remote.mirror.get(path) {
                        extra = format!(
                            "X-Mock-Size: {}\r\nx-amz-meta-vesper-blake3: {}\r\n",
                            value.len(),
                            remote
                                .mirror_hashes
                                .get(path)
                                .map(String::as_str)
                                .unwrap_or("")
                        );
                        value.clone()
                    } else {
                        status = "404 Not Found";
                        Vec::new()
                    }
                } else if path.ends_with("/releases/tags/vesper-metadata") {
                    if !remote.metadata_created {
                        status = "404 Not Found";
                    }
                    serde_json::to_vec(&metadata_release).unwrap()
                } else if path == "/metadata-assets" {
                    let assets: Vec<_> = names.iter().enumerate().filter_map(|(id, name)| remote.metadata_assets.get(*name).map(|bytes| json!({"id":100+id,"name":name,"size":bytes.len(),"browser_download_url":format!("{origin}/metadata/{name}")}))).collect();
                    serde_json::to_vec(&assets).unwrap()
                } else if method == "DELETE" && path.contains("/releases/assets/") {
                    let id: usize = path.rsplit('/').next().unwrap().parse().unwrap();
                    remote.metadata_assets.remove(names[id - 100]);
                    status = "204 No Content";
                    Vec::new()
                } else if method == "POST" && path.starts_with("/metadata-uploads?name=") {
                    remote.metadata_assets.insert(
                        path.trim_start_matches("/metadata-uploads?name=").into(),
                        body,
                    );
                    if remote.disconnect_metadata_once {
                        remote.disconnect_metadata_once = false;
                        continue;
                    }
                    if remote.reject_metadata {
                        status = "503 Unavailable";
                    }
                    b"{}".to_vec()
                } else if path.ends_with("/releases/tags/r2") {
                    if !remote.created {
                        status = "404 Not Found";
                    }
                    serde_json::to_vec(&release).unwrap()
                } else if path.ends_with("/releases/tags/r1") {
                    serde_json::to_vec(&json!({"id":1,"tag_name":"r1","upload_url":"unused","assets_url":format!("{origin}/old-assets")})).unwrap()
                } else if method == "POST" && path.ends_with("/releases") {
                    if serde_json::from_slice::<Value>(&body).unwrap()["tag_name"]
                        == "vesper-metadata"
                    {
                        remote.metadata_created = true;
                        serde_json::to_vec(&metadata_release).unwrap()
                    } else {
                        assert_eq!(
                            serde_json::from_slice::<Value>(&body).unwrap()["tag_name"],
                            "r2"
                        );
                        remote.created = true;
                        serde_json::to_vec(&release).unwrap()
                    }
                } else if method == "POST" && path.starts_with("/uploads?name=") {
                    let name = path.trim_start_matches("/uploads?name=");
                    remote.assets.insert(name.into(), body);
                    b"{}".to_vec()
                } else if path == "/assets" || path == "/old-assets" {
                    let source = if path == "/old-assets" {
                        &remote.old_assets
                    } else {
                        &remote.assets
                    };
                    let assets: Vec<_> = source.iter().enumerate().map(|(id, (name, bytes))| json!({"id":id,"name":name,"size":bytes.len(),"browser_download_url":format!("{origin}/download/{}/{name}", if path == "/old-assets" { "r1" } else { "r2" })})).collect();
                    serde_json::to_vec(&assets).unwrap()
                } else if let Some(name) = path.strip_prefix("/download/") {
                    assert!(!header.to_ascii_lowercase().contains("authorization:"));
                    let (release, name) = name.split_once('/').unwrap();
                    let bytes = if release == "r1" {
                        &remote.old_assets
                    } else {
                        &remote.assets
                    }
                    .get(name)
                    .unwrap();
                    if method == "HEAD" {
                        let result = bytes.clone();
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            result.len()
                        )
                        .unwrap();
                        continue;
                    }
                    let range = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("range: bytes=")
                                .map(str::to_owned)
                        })
                        .unwrap();
                    let (start, end) = range.split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end = end.parse::<usize>().unwrap().min(bytes.len() - 1);
                    status = "206 Partial Content";
                    extra = format!("Content-Range: bytes {start}-{end}/{}\r\n", bytes.len());
                    bytes[start..=end].to_vec()
                } else {
                    panic!("unexpected mock request: {method} {path}")
                };
                drop(remote);
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    result.len()
                )
                .unwrap();
                if method != "HEAD" {
                    let _ = stream.write_all(&result);
                }
            }
        });
        Self {
            base,
            remote,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for GitHubMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn vesper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .env("VESPER_DEV_KEY", "13".repeat(32))
        .env("VESPER_APP_ID", "test.publish")
        .output()
        .unwrap()
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_index(index: &Path, channel: &str) {
    let targets: Value =
        serde_json::from_slice(&fs::read(index.join("apps/test.publish/targets.json")).unwrap())
            .unwrap();
    assert_eq!(
        targets["signed"]["channels"][channel]["manifest"]["release_tag"],
        "r2"
    );
    assert_eq!(
        targets["signed"]["sources"][0]["url_template"],
        "https://github.com/owner/repo/releases/download/{release}/{pack_id}"
    );
}

#[test]
fn publish_self_hosted_creates_and_replaces_metadata_release_assets() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let build = temp.path().join("build");
    let metadata = temp.path().join("metadata");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.bin"), vec![42; 8192]).unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("store.json"), b"{}").unwrap();
    fs::write(metadata.join("description.md"), b"First description").unwrap();
    assert_success(vesper(&[
        "pack",
        "--input",
        input.to_str().unwrap(),
        "--output",
        build.to_str().unwrap(),
        "--app-id",
        "test.publish",
        "--platform",
        "windows-x64",
        "--version",
        "r2",
    ]));
    let server = GitHubMock::new();
    let args = [
        "publish",
        build.to_str().unwrap(),
        "--metadata-dir",
        metadata.to_str().unwrap(),
        "--owner",
        "owner",
        "--repo",
        "repo",
        "--token",
        "mock-token",
        "--api-base",
        &server.base,
        "--upload-base",
        &server.base,
    ];
    server.remote.lock().unwrap().disconnect_metadata_once = true;
    server.remote.lock().unwrap().disconnect_api_once = true;
    let first = vesper(&args);
    let stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(stdout
        .contains("metadata_url=https://github.com/owner/repo/releases/download/vesper-metadata/"));
    assert!(stdout.contains("targets_version=1"), "{stdout}");
    assert!(!stdout.contains("commit this checkout"), "{stdout}");
    assert_success(first);
    {
        let remote = server.remote.lock().unwrap();
        let created: Vec<serde_json::Value> = remote
            .requests
            .iter()
            .filter(|(method, path, _)| method == "POST" && path.ends_with("/releases"))
            .map(|(_, _, body)| serde_json::from_slice(body).unwrap())
            .collect();
        assert_eq!(created.len(), 2);
        assert_eq!(remote.metadata_assets["store.json"], b"{}");
        assert!(remote
            .requests
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "/metadata-uploads?name=store.json")
            .all(|(_, _, body)| body == b"{}"));
        for release in created {
            // Only the mutable metadata release opts out of becoming "Latest".
            let metadata = release["tag_name"] == "vesper-metadata";
            assert_eq!(release.get("make_latest").is_some(), metadata, "{release}");
            if metadata {
                assert_eq!(release["make_latest"], "false");
            }
        }
    }
    let old = server.remote.lock().unwrap().metadata_assets.clone();
    assert_eq!(old.len(), 3);
    fs::write(metadata.join("description.md"), b"Second description").unwrap();
    assert_success(vesper(&args));
    let remote = server.remote.lock().unwrap();
    assert_eq!(remote.metadata_assets.len(), 3);
    let targets: vesper_meta::Signed<vesper_meta::AppTargetsSigned> =
        serde_json::from_slice(&remote.metadata_assets["targets.json"]).unwrap();
    assert_eq!(targets.signed.version, 2);
    vesper_meta::verify_store_bytes(
        &targets.signed,
        "description.md",
        &remote.metadata_assets["description.md"],
    )
    .unwrap();
    assert_eq!(
        remote
            .requests
            .iter()
            .filter(|(method, _, _)| method == "DELETE")
            .count(),
        4
    );
    assert_eq!(
        remote.metadata_assets["description.md"],
        b"Second description"
    );
    assert_eq!(remote.assets.len(), 2);
    assert_eq!(
        remote
            .requests
            .iter()
            .filter(|(method, path, _)| method == "POST" && path.ends_with("/releases"))
            .count(),
        2
    );
    assert_ne!(old["targets.json"], remote.metadata_assets["targets.json"]);
    drop(remote);
    server.remote.lock().unwrap().reject_metadata = true;
    let failed = vesper(&args);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr)
        .contains("local targets v3 is signed but unpublished; rerun publish"));
}

#[test]
fn publish_tag_and_publish_github_share_index_and_reject_ambiguous_tag_before_requests() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let build = temp.path().join("build");
    let index = temp.path().join("index");
    fs::create_dir_all(index.join("apps/test.publish")).unwrap();
    fs::write(index.join("apps/test.publish/store.json"), b"{}").unwrap();
    fs::write(
        index.join("apps/test.publish/description.md"),
        b"description",
    )
    .unwrap();
    let old_build = temp.path().join("old-build");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.bin"), vec![42u8; 8192]).unwrap();
    assert_success(vesper(&[
        "pack",
        "--input",
        input.to_str().unwrap(),
        "--output",
        old_build.to_str().unwrap(),
        "--app-id",
        "test.publish",
        "--platform",
        "windows-x64",
        "--version",
        "1",
        "--release-tag",
        "r1",
    ]));
    fs::write(input.join("new.bin"), vec![73u8; 8192]).unwrap();
    let input = input.to_str().unwrap();
    let build = build.to_str().unwrap();
    let index_str = index.to_str().unwrap();
    assert_success(vesper(&[
        "pack",
        "--input",
        input,
        "--output",
        build,
        "--app-id",
        "test.publish",
        "--platform",
        "windows-x64",
        "--version",
        "2",
        "--release-tag",
        "r2",
        "--previous",
        old_build.join("manifest.cbor.zst").to_str().unwrap(),
    ]));
    let server = GitHubMock::new();
    for file in fs::read_dir(old_build.join("packs")).unwrap() {
        let file = file.unwrap();
        server.remote.lock().unwrap().old_assets.insert(
            file.file_name().into_string().unwrap(),
            fs::read(file.path()).unwrap(),
        );
    }
    let args = [
        "publish",
        build,
        "--index",
        index_str,
        "--owner",
        "owner",
        "--repo",
        "repo",
        "--token",
        "mock-token",
        "--api-base",
        &server.base,
        "--upload-base",
        &server.base,
    ];
    let missing = vesper(&args);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--tag"));
    assert!(server.remote.lock().unwrap().requests.is_empty());
    let mut explicit = args.to_vec();
    explicit.extend(["--tag", "r2"]);
    assert_success(vesper(&explicit));
    assert_index(&index, "stable");
    let remote = server.remote.lock().unwrap();
    let creates: Vec<_> = remote
        .requests
        .iter()
        .filter(|(method, path, _)| method == "POST" && path.ends_with("/releases"))
        .collect();
    assert_eq!(creates.len(), 1);
    assert_eq!(remote.assets.len(), 2);
    assert!(!remote.old_assets.is_empty());
    assert!(remote
        .old_assets
        .keys()
        .all(|name| !remote.assets.contains_key(name)));
    assert!(remote.assets.contains_key("manifest.cbor.zst"));
    assert_eq!(
        remote
            .assets
            .keys()
            .filter(|name| name.ends_with(".vpack"))
            .count(),
        1
    );
    drop(remote);
    // The other command uses the same writer; endpoint overrides keep this test local.
    assert_success(vesper(&[
        "publish-github",
        "--owner",
        "owner",
        "--repo",
        "repo",
        "--tag",
        "r2",
        "--manifest",
        &format!("{build}/manifest.cbor.zst"),
        "--packs-dir",
        &format!("{build}/packs"),
        "--token",
        "mock-token",
        "--api-base",
        &server.base,
        "--upload-base",
        &server.base,
        "--index",
        index_str,
        "--channel",
        "beta",
    ]));
    assert_index(&index, "beta");
    // A rerun needs no local copy of packs already available anonymously.
    for file in fs::read_dir(format!("{build}/packs")).unwrap() {
        fs::remove_file(file.unwrap().path()).unwrap();
    }
    let rerun = vesper(&explicit);
    assert!(
        rerun.status.success(),
        "{}",
        String::from_utf8_lossy(&rerun.stderr)
    );
    assert!(String::from_utf8_lossy(&rerun.stdout).contains("uploaded=0 skipped_existing=2"));
    let mut remote = server.remote.lock().unwrap();
    let pack = remote
        .assets
        .keys()
        .find(|name| name.ends_with(".vpack"))
        .unwrap()
        .clone();
    let pack_bytes = remote.assets[&pack].clone();
    remote.assets.insert(pack.clone(), vec![0]);
    drop(remote);
    let wrong_size = vesper(&explicit);
    assert!(!wrong_size.status.success());
    assert!(String::from_utf8_lossy(&wrong_size.stderr).contains("asset size mismatch"));
    server
        .remote
        .lock()
        .unwrap()
        .assets
        .insert(pack.clone(), pack_bytes);
    server.remote.lock().unwrap().assets.remove(&pack);
    let missing = vesper(&explicit);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing from release"));
}

#[test]
fn r2_publish_mirrors_old_packs_and_preserves_targets_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let old = temp.path().join("old");
    let build = temp.path().join("build");
    let index = temp.path().join("index");
    let metadata = index.join("apps/test.publish");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("store.json"), b"{}").unwrap();
    fs::write(metadata.join("description.md"), b"Mirror test").unwrap();
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("old.bin"), vec![42; 8192]).unwrap();
    assert_success(vesper(&[
        "pack",
        "--input",
        input.to_str().unwrap(),
        "--output",
        old.to_str().unwrap(),
        "--app-id",
        "test.publish",
        "--platform",
        "windows-x64",
        "--version",
        "1",
        "--release-tag",
        "r1",
    ]));
    fs::write(input.join("new.bin"), vec![73; 8192]).unwrap();
    assert_success(vesper(&[
        "pack",
        "--input",
        input.to_str().unwrap(),
        "--output",
        build.to_str().unwrap(),
        "--app-id",
        "test.publish",
        "--platform",
        "windows-x64",
        "--version",
        "2",
        "--release-tag",
        "r2",
        "--previous",
        old.join("manifest.cbor.zst").to_str().unwrap(),
    ]));
    let server = GitHubMock::new();
    for file in fs::read_dir(old.join("packs")).unwrap() {
        let file = file.unwrap();
        server.remote.lock().unwrap().old_assets.insert(
            file.file_name().to_string_lossy().into_owned(),
            fs::read(file.path()).unwrap(),
        );
    }
    let mut args = vec![
        "publish",
        build.to_str().unwrap(),
        "--tag",
        "r2",
        "--index",
        index.to_str().unwrap(),
        "--owner",
        "owner",
        "--repo",
        "repo",
        "--token",
        "mock-token",
        "--api-base",
        &server.base,
        "--upload-base",
        &server.base,
        "--r2-bucket",
        "mirror",
        "--r2-endpoint",
        &server.base,
        "--r2-prefix",
        "game",
        "--r2-public-base",
        "https://cdn.example.com/game",
    ];
    let invoke = |args: &[&str], credentials: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vesper"));
        command
            .args(args)
            .current_dir(temp.path())
            .env("VESPER_DEV_KEY", "13".repeat(32));
        if credentials {
            command
                .env("VESPER_R2_ACCESS_KEY_ID", "mock-access")
                .env("VESPER_R2_SECRET_ACCESS_KEY", "mock-secret");
        } else {
            command
                .env_remove("VESPER_R2_ACCESS_KEY_ID")
                .env_remove("VESPER_R2_SECRET_ACCESS_KEY");
        }
        command.output().unwrap()
    };
    let missing = invoke(&args, false);
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("VESPER_R2_ACCESS_KEY_ID"));
    assert!(!error.contains("mock-secret"));
    assert!(server.remote.lock().unwrap().requests.is_empty());
    args.push("--dry-run");
    let dry = invoke(&args, true);
    assert!(String::from_utf8_lossy(&dry.stdout).contains("would-recover-from-github game/r1/"));
    assert_success(dry);
    assert!(!metadata.join("targets.json").exists());
    assert!(server
        .remote
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|(m, _, _)| m == "GET" || m == "HEAD"));
    assert!(server.remote.lock().unwrap().mirror.is_empty());
    assert!(server
        .remote
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|(method, path, _)| method != "GET" || !path.starts_with("/download/")));
    args.pop();
    assert_success(invoke(&args, true));
    let targets = fs::read(metadata.join("targets.json")).unwrap();
    let value: Value = serde_json::from_slice(&targets).unwrap();
    assert_eq!(value["signed"]["sources"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["signed"]["sources"][1]["url_template"],
        "https://cdn.example.com/game/{release}/{pack_id}"
    );
    {
        let remote = server.remote.lock().unwrap();
        assert!(remote
            .mirror
            .contains_key("/mirror/game/r2/manifest.cbor.zst"));
        for (name, bytes) in &remote.old_assets {
            assert_eq!(remote.mirror[&format!("/mirror/game/r1/{name}")], *bytes);
        }
    }
    // A corrupt recovered old pack fails its hash check before targets change.
    {
        let mut remote = server.remote.lock().unwrap();
        remote.mirror.clear();
        let bytes = remote.old_assets.values_mut().next().unwrap();
        bytes[0] ^= 1;
    }
    let failed = invoke(&args, true);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("length/hash mismatch"));
    assert_eq!(fs::read(metadata.join("targets.json")).unwrap(), targets);
    // Missing objects also leave the signed publication untouched.
    server.remote.lock().unwrap().old_assets.clear();
    assert!(!invoke(&args, true).status.success());
    assert_eq!(fs::read(metadata.join("targets.json")).unwrap(), targets);
}

#[test]
fn r2_config_cli_overrides_file_and_self_hosted_sources() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let build = temp.path().join("build");
    let metadata = temp.path().join("metadata");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("game.bin"), vec![7; 8192]).unwrap();
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("store.json"), b"{}").unwrap();
    fs::write(metadata.join("description.md"), b"Mirror test").unwrap();
    assert_success(vesper(&[
        "pack",
        "--input",
        input.to_str().unwrap(),
        "--output",
        build.to_str().unwrap(),
        "--app-id",
        "test.publish",
        "--platform",
        "windows-x64",
        "--version",
        "r2",
    ]));
    let server = GitHubMock::new();
    fs::write(temp.path().join("vesper.toml"), format!("app_id = 'test.publish'\n[r2]\nbucket = 'wrong'\nendpoint = '{}'\nprefix = 'game'\npublic_base = 'https://cdn.example.com/game'\n", server.base)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .current_dir(temp.path())
        .args([
            "publish",
            build.to_str().unwrap(),
            "--metadata-dir",
            metadata.to_str().unwrap(),
            "--owner",
            "owner",
            "--repo",
            "repo",
            "--token",
            "mock-token",
            "--api-base",
            &server.base,
            "--upload-base",
            &server.base,
            "--r2-bucket",
            "mirror",
        ])
        .env("VESPER_DEV_KEY", "13".repeat(32))
        .env("VESPER_R2_ACCESS_KEY_ID", "mock-access")
        .env("VESPER_R2_SECRET_ACCESS_KEY", "mock-secret")
        .output()
        .unwrap();
    assert_success(output);
    let remote = server.remote.lock().unwrap();
    let targets: Value = serde_json::from_slice(&remote.metadata_assets["targets.json"]).unwrap();
    assert_eq!(targets["signed"]["sources"][0]["priority"], 0);
    assert_eq!(targets["signed"]["sources"][1]["kind"], "r2");
    assert!(remote
        .mirror
        .contains_key("/mirror/game/r2/manifest.cbor.zst"));
}
#[test]
fn r2_all_channels_are_verified_before_targets_and_promote_requires_publish() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input");
    let old = temp.path().join("old");
    let current = temp.path().join("current");
    let index = temp.path().join("index");
    let metadata = index.join("apps/test.publish");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join("store.json"), b"{}").unwrap();
    fs::write(metadata.join("description.md"), b"All channels").unwrap();
    fs::create_dir_all(&input).unwrap();
    let pack = |output: &Path, version: &str| {
        assert_success(vesper(&[
            "pack",
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--app-id",
            "test.publish",
            "--platform",
            "windows-x64",
            "--version",
            version,
        ]));
    };
    fs::write(input.join("game.bin"), vec![42; 8192]).unwrap();
    pack(&old, "r1");
    assert_success(vesper(&[
        "publish",
        old.to_str().unwrap(),
        "--index",
        index.to_str().unwrap(),
        "--channel",
        "beta",
    ]));
    fs::write(input.join("game.bin"), vec![73; 8192]).unwrap();
    pack(&current, "r2");
    let server = GitHubMock::new();
    let old_manifest = fs::read(old.join("manifest.cbor.zst")).unwrap();
    {
        let mut remote = server.remote.lock().unwrap();
        remote
            .old_assets
            .insert("manifest.cbor.zst".into(), old_manifest.clone());
        for file in fs::read_dir(old.join("packs")).unwrap() {
            let file = file.unwrap();
            remote.old_assets.insert(
                file.file_name().to_string_lossy().into_owned(),
                fs::read(file.path()).unwrap(),
            );
        }
    }
    let invoke = |dry: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vesper"));
        command
            .current_dir(temp.path())
            .args([
                "publish",
                current.to_str().unwrap(),
                "--index",
                index.to_str().unwrap(),
                "--owner",
                "owner",
                "--repo",
                "repo",
                "--token",
                "mock-token",
                "--api-base",
                &server.base,
                "--upload-base",
                &server.base,
                "--r2-bucket",
                "mirror",
                "--r2-endpoint",
                &server.base,
                "--r2-public-base",
                "https://cdn.example.com",
                "--channel",
                "stable",
            ])
            .env("VESPER_DEV_KEY", "13".repeat(32))
            .env("VESPER_R2_ACCESS_KEY_ID", "mock-access")
            .env("VESPER_R2_SECRET_ACCESS_KEY", "mock-secret");
        if dry {
            command.arg("--dry-run");
        }
        command.output().unwrap()
    };
    assert_success(invoke(true));
    assert!(server
        .remote
        .lock()
        .unwrap()
        .requests
        .iter()
        .all(|(m, p, _)| m != "GET" || !p.starts_with("/download/")));
    assert_success(invoke(false));
    let targets = fs::read(metadata.join("targets.json")).unwrap();
    {
        let remote = server.remote.lock().unwrap();
        for (name, bytes) in &remote.old_assets {
            assert_eq!(&remote.mirror[&format!("/mirror/r1/{name}")], bytes);
        }
        assert!(remote.mirror.contains_key("/mirror/r2/manifest.cbor.zst"));
    }
    let promoted = vesper(&[
        "promote",
        "beta",
        "stable",
        "--index",
        index.to_str().unwrap(),
    ]);
    assert!(!promoted.status.success());
    assert!(String::from_utf8_lossy(&promoted.stderr).contains("mirror verification"));
    assert_eq!(fs::read(metadata.join("targets.json")).unwrap(), targets);
    {
        let mut remote = server.remote.lock().unwrap();
        remote.mirror.clear();
        remote.old_assets.get_mut("manifest.cbor.zst").unwrap()[0] ^= 1;
    }
    let failed = invoke(false);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("length/hash mismatch"));
    assert_eq!(fs::read(metadata.join("targets.json")).unwrap(), targets);
    {
        let mut remote = server.remote.lock().unwrap();
        remote
            .old_assets
            .insert("manifest.cbor.zst".into(), old_manifest);
        remote
            .old_assets
            .retain(|name, _| !name.ends_with(".vpack"));
    }
    assert!(!invoke(false).status.success());
    assert_eq!(fs::read(metadata.join("targets.json")).unwrap(), targets);
}
