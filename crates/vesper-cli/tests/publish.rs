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
    created: bool,
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
                let release = json!({"id":2,"tag_name":"r2","upload_url":format!("{origin}/uploads{{?name,label}}"),"assets_url":format!("{origin}/assets")});
                let mut status = "200 OK";
                let mut extra = String::new();
                let result = if path.ends_with("/releases/tags/r2") {
                    if !remote.created {
                        status = "404 Not Found";
                    }
                    serde_json::to_vec(&release).unwrap()
                } else if path.ends_with("/releases/tags/r1") {
                    serde_json::to_vec(&json!({"id":1,"tag_name":"r1","upload_url":"unused","assets_url":format!("{origin}/old-assets")})).unwrap()
                } else if method == "POST" && path.ends_with("/releases") {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&body).unwrap()["tag_name"],
                        "r2"
                    );
                    remote.created = true;
                    serde_json::to_vec(&release).unwrap()
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
                    let assets: Vec<_> = source.iter().enumerate().map(|(id, (name, bytes))| json!({"id":id,"name":name,"size":bytes.len(),"browser_download_url":format!("{origin}/download/{name}")})).collect();
                    serde_json::to_vec(&assets).unwrap()
                } else if let Some(name) = path.strip_prefix("/download/") {
                    assert!(!header.to_ascii_lowercase().contains("authorization:"));
                    let bytes = remote
                        .assets
                        .get(name)
                        .or_else(|| remote.old_assets.get(name))
                        .unwrap();
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
                stream.write_all(&result).unwrap();
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
