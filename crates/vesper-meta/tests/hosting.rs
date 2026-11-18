use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use vesper_meta::{hosting::*, *};

type Response = (String, Vec<u8>);
/// A listing that passes every store rule.
const STORE: &[u8] =
    include_bytes!("../../../index-template/test/fixtures/store/valid-minimal.json");
struct Mock {
    base: String,
    files: Arc<Mutex<BTreeMap<String, Response>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Mock {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(Mutex::new(BTreeMap::<String, Response>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let data = files.clone();
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                // Accepted sockets inherit non-blocking mode on Windows.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 8192];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&request);
                let path = text.split_whitespace().nth(1).unwrap_or("");
                let (headers, bytes) = data
                    .lock()
                    .unwrap()
                    .get(path)
                    .cloned()
                    .unwrap_or(("404 Not Found\r\n".into(), Vec::new()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(&bytes);
            }
        });
        Self {
            base,
            files,
            stop,
            worker: Some(worker),
        }
    }
    fn put(&self, name: &str, bytes: Vec<u8>) {
        self.files
            .lock()
            .unwrap()
            .insert(format!("/{name}"), ("200 OK\r\n".into(), bytes));
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn publish(mock: &Mock, key: &KeyPair, version: u64) -> Signed<AppTargetsSigned> {
    let targets = sign(
        AppTargetsSigned {
            typ: "targets".into(),
            spec_version: "1.0.0".into(),
            version,
            expires: expires_in(10),
            channels: BTreeMap::new(),
            sources: vec![AppSource {
                id: "test".into(),
                kind: "http".into(),
                url_template: "https://example.test/{release}/{pack_id}".into(),
                priority: 0,
            }],
            metadata: [
                ("store.json".into(), store_file_description(STORE)),
                ("description.md".into(), store_file_description(b"Game")),
            ]
            .into(),
        },
        &[key],
    )
    .unwrap();
    mock.put("targets.json", canonical_bytes(&targets).unwrap());
    mock.put("store.json", STORE.to_vec());
    mock.put("description.md", b"Game".to_vec());
    targets
}
fn route(mock: &Mock, key: &KeyPair) -> AppRoute {
    AppRoute {
        keys: vec![key.public_hex()],
        threshold: 1,
        metadata_url: Some(mock.base.clone()),
    }
}

#[test]
fn aggregate_normal_update_and_unchanged_preserve_online_roles() {
    let mock = Mock::new();
    let key = KeyPair::generate();
    let temp = tempfile::tempdir().unwrap();
    let route = route(&mock, &key);
    publish(&mock, &key, 1);
    assert_eq!(
        aggregate_app(temp.path(), "game", &route).unwrap(),
        "updated"
    );
    fs::write(temp.path().join("apps/game/snapshot.json"), "online role").unwrap();
    assert_eq!(
        aggregate_app(temp.path(), "game", &route).unwrap(),
        "unchanged"
    );
    publish(&mock, &key, 2);
    assert_eq!(
        aggregate_app(temp.path(), "game", &route).unwrap(),
        "updated"
    );
    assert_eq!(
        fs::read(temp.path().join("apps/game/snapshot.json")).unwrap(),
        b"online role"
    );
}

fn rejected_mutation(mutate: impl FnOnce(&Mock, &KeyPair)) -> String {
    let mock = Mock::new();
    let key = KeyPair::generate();
    let temp = tempfile::tempdir().unwrap();
    let route = route(&mock, &key);
    publish(&mock, &key, 2);
    aggregate_app(temp.path(), "game", &route).unwrap();
    let old: Vec<_> = FILES
        .iter()
        .map(|name| fs::read(temp.path().join("apps/game").join(name)).unwrap())
        .collect();
    mutate(&mock, &key);
    let error = format!(
        "{:#}",
        aggregate_app(temp.path(), "game", &route).unwrap_err()
    );
    for (name, bytes) in FILES.iter().zip(old) {
        assert_eq!(
            fs::read(temp.path().join("apps/game").join(name)).unwrap(),
            bytes
        );
    }
    error
}
#[test]
fn aggregate_rejects_wrong_signature_and_preserves_old_bundle() {
    let error = rejected_mutation(|mock, _| {
        publish(mock, &KeyPair::generate(), 3);
    });
    assert!(error.contains("signature"), "{error}");
}
#[test]
fn aggregate_rejects_store_binding_mismatch() {
    assert!(rejected_mutation(|mock, key| {
        publish(mock, key, 3);
        mock.put("store.json", b"tampered".to_vec());
    })
    .contains("mismatch"));
}

#[test]
fn aggregate_rejects_signed_unsafe_catalogue_tags() {
    let error = rejected_mutation(|mock, key| {
        let mut targets = publish(mock, key, 3);
        let store = br#"{"tags":["../../outside"]}"#;
        targets
            .signed
            .metadata
            .insert("store.json".into(), store_file_description(store));
        mock.put(
            "targets.json",
            canonical_bytes(&sign(targets.signed, &[key]).unwrap()).unwrap(),
        );
        mock.put("store.json", store.to_vec());
    });
    assert!(error.contains("invalid catalogue tags"), "{error}");
}
#[test]
fn aggregate_rejects_version_rollback() {
    assert!(rejected_mutation(|mock, key| {
        publish(mock, key, 1);
    })
    .contains("rollback"));
}
#[test]
fn aggregate_rejects_same_version_different_content() {
    assert!(rejected_mutation(|mock, key| {
        let mut targets = publish(mock, key, 2);
        targets.signed.expires += 1;
        mock.put(
            "targets.json",
            canonical_bytes(&sign(targets.signed, &[key]).unwrap()).unwrap(),
        );
    })
    .contains("same targets version"));
}
#[test]
fn aggregate_unreachable_retains_old_data() {
    assert!(rejected_mutation(|mock, _| {
        mock.files.lock().unwrap().clear();
    })
    .contains("404"));
}

#[test]
fn aggregate_connection_failure_retains_old_data() {
    let mock = Mock::new();
    let key = KeyPair::generate();
    let temp = tempfile::tempdir().unwrap();
    let mut route = route(&mock, &key);
    publish(&mock, &key, 2);
    aggregate_app(temp.path(), "game", &route).unwrap();
    let path = temp.path().join("apps/game/targets.json");
    let old = fs::read(&path).unwrap();
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    route.metadata_url = Some(format!("http://{}/", closed.local_addr().unwrap()));
    drop(closed);
    assert!(aggregate_app(temp.path(), "game", &route).is_err());
    assert_eq!(fs::read(path).unwrap(), old);
}
#[test]
fn aggregate_rejects_oversized_metadata() {
    assert!(rejected_mutation(|mock, _| {
        mock.put("targets.json", vec![b' '; MAX_FILE_SIZE as usize + 1]);
    })
    .contains("1 MiB"));
}
#[test]
fn aggregate_rejects_non_https_redirect() {
    let error = rejected_mutation(|mock, _| {
        mock.files.lock().unwrap().insert(
            "/targets.json".into(),
            (
                "302 Found\r\nLocation: http://localhost:1/targets.json\r\n".into(),
                Vec::new(),
            ),
        );
    });
    assert!(error.contains("redirect requires HTTPS"), "{error}");
}
#[test]
fn metadata_url_policy_and_production_redirects() {
    for url in [
        "http://example.test/",
        "http://localhost/",
        "ftp://example.test/",
        "https://user:pass@example.test/",
        "https://example.test/?query",
    ] {
        assert!(metadata_base(url).is_err(), "{url}");
    }
    assert!(metadata_base("https://example.test/base").is_ok());
    assert!(metadata_base("http://127.0.0.1:1234/base").is_ok());
}

#[test]
fn aggregate_copies_signed_documents_and_drops_unsigned_ones() {
    let mock = Mock::new();
    let key = KeyPair::generate();
    let temp = tempfile::tempdir().unwrap();
    let route = route(&mock, &key);
    let mut targets = publish(&mock, &key, 1);
    let changelog = b"## 1.0
- First release
";
    targets
        .signed
        .metadata
        .insert("changelog.md".into(), store_file_description(changelog));
    mock.put(
        "targets.json",
        canonical_bytes(&sign(targets.signed, &[&key]).unwrap()).unwrap(),
    );
    mock.put("changelog.md", changelog.to_vec());
    assert_eq!(
        aggregate_app(temp.path(), "game", &route).unwrap(),
        "updated"
    );
    let dir = temp.path().join("apps/game");
    assert_eq!(fs::read(dir.join("changelog.md")).unwrap(), changelog);
    // A later version that no longer signs the changelog removes the copy.
    publish(&mock, &key, 2);
    assert_eq!(
        aggregate_app(temp.path(), "game", &route).unwrap(),
        "updated"
    );
    assert!(!dir.join("changelog.md").exists());
    assert!(dir.join("store.json").exists() && dir.join("description.md").exists());
}

#[test]
fn aggregate_rejects_listings_that_break_store_rules() {
    let error = rejected_mutation(|mock, key| {
        let mut targets = publish(mock, key, 3);
        let mut store: serde_json::Value = serde_json::from_slice(STORE).unwrap();
        store["trailers"] = serde_json::json!([{"kind": "vimeo", "id": "1"}]);
        let store = serde_json::to_vec(&store).unwrap();
        targets
            .signed
            .metadata
            .insert("store.json".into(), store_file_description(&store));
        mock.put(
            "targets.json",
            canonical_bytes(&sign(targets.signed, &[key]).unwrap()).unwrap(),
        );
        mock.put("store.json", store);
    });
    assert!(
        error.contains("invalid store.json") && error.contains("trailers[0]"),
        "{error}"
    );
}
