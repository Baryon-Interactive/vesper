use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use vesper_meta::*;

const SECRET: &str = "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a";

fn cli(index: &Path, command: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["index", command, "--index", index.to_str().unwrap()])
        .args(args)
        .env("VESPER_ONLINE_KEY", SECRET)
        .args(if command == "snapshot" {
            vec!["--initialize"]
        } else {
            vec![]
        })
        .output()
        .unwrap()
}
fn success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn rejected(output: Output, link: &str) {
    assert!(!output.status.success(), "broken metadata was accepted");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(link),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn fixture(index: &Path) -> KeyPair {
    let key = KeyPair::from_secret_hex(SECRET).unwrap();
    fs::create_dir_all(index.join("apps/demo")).unwrap();
    let root = sign(
        RootSigned {
            typ: "root".into(),
            spec_version: "1.0.0".into(),
            version: 1,
            expires: expires_in(90),
            keys: BTreeMap::from([(key.keyid.clone(), key.public_hex())]),
            roles: ["root", "snapshot", "timestamp"]
                .into_iter()
                .map(|role| {
                    (
                        role.into(),
                        RoleSpec {
                            keyids: vec![key.keyid.clone()],
                            threshold: 1,
                        },
                    )
                })
                .collect(),
        },
        &[&key],
    )
    .unwrap();
    write_json(&index.join("root.json"), &root).unwrap();
    write_json(
        &index.join("routes.json"),
        &sign(
            RoutesSigned {
                typ: "routes".into(),
                spec_version: "1.0.0".into(),
                version: 1,
                expires: expires_in(90),
                apps: BTreeMap::from([(
                    "demo".into(),
                    AppRoute {
                        keys: vec![key.public_hex()],
                        threshold: 1,
                        metadata_url: None,
                    },
                )]),
            },
            &[&key],
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(index.join("apps/demo/store.json"), b"{}").unwrap();
    fs::write(index.join("apps/demo/description.md"), b"{}").unwrap();
    targets(index, &key, 7);
    key
}
fn targets(index: &Path, key: &KeyPair, version: u64) {
    write_json(
        &index.join("apps/demo/targets.json"),
        &sign(
            AppTargetsSigned {
                metadata: ["store.json", "description.md"]
                    .into_iter()
                    .map(|name| (name.into(), vesper_meta::store_file_description(b"{}")))
                    .collect(),
                typ: "targets".into(),
                spec_version: "1.0.0".into(),
                version,
                expires: expires_in(90),
                channels: BTreeMap::new(),
                sources: vec![AppSource {
                    id: "mock".into(),
                    kind: "http".into(),
                    url_template: "https://example.test/{release}/{pack_id}".into(),
                    priority: 0,
                }],
            },
            &[key],
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn store_changes_require_resigned_targets_in_both_verification_modes() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path();
    fixture(index);
    success(cli(index, "snapshot", &["--app", "demo"]));
    let dir = index.join("apps/demo");
    let original = fs::read(dir.join("targets.json")).unwrap();
    for name in ["store.json", "description.md"] {
        fs::write(dir.join(name), b"changed store data").unwrap();
        rejected(cli(index, "verify", &[]), "mismatch");
        rejected(cli(index, "verify", &["--targets-only"]), "mismatch");
        let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args([
                "sign",
                "--targets",
                dir.join("targets.json").to_str().unwrap(),
            ])
            .env("VESPER_DEV_KEY", SECRET)
            .output()
            .unwrap();
        success(output);
        success(cli(index, "verify", &["--targets-only"]));
        fs::write(dir.join(name), b"{}").unwrap();
        fs::write(dir.join("targets.json"), &original).unwrap();
    }
    let mut old: serde_json::Value = serde_json::from_slice(&original).unwrap();
    old["signed"].as_object_mut().unwrap().remove("metadata");
    fs::write(dir.join("targets.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    rejected(cli(index, "verify", &["--targets-only"]), "metadata");
}

#[test]
fn previous_rejects_online_role_rollback_and_deletion_even_with_targets_only() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index");
    let base = temp.path().join("base");
    fixture(&index);
    fixture(&base);
    success(cli(&index, "snapshot", &["--app", "demo"]));
    success(cli(&base, "snapshot", &["--app", "demo"]));
    for name in ["snapshot.json", "timestamp.json"] {
        let path = index.join("apps/demo").join(name);
        let original = fs::read(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["signed"]["version"] = 0.into();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        for mode in [vec![], vec!["--targets-only"]] {
            let mut args = vec!["--previous", base.to_str().unwrap()];
            args.extend(mode);
            rejected(cli(&index, "verify", &args), "version rollback");
        }
        fs::remove_file(&path).unwrap();
        rejected(
            cli(
                &index,
                "verify",
                &["--previous", base.to_str().unwrap(), "--targets-only"],
            ),
            "missing",
        );
        fs::write(path, original).unwrap();
    }
}

#[test]
fn online_signing_requires_history_and_recovers_deleted_versions() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index");
    let key = fixture(&index);
    let run = |command: &str, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args(["index", command, "--index", index.to_str().unwrap()])
            .args(args)
            .env("VESPER_ONLINE_KEY", SECRET)
            .output()
            .unwrap()
    };
    rejected(
        run("snapshot", &["--app", "demo"]),
        "no previous snapshot.json",
    );
    success(run("snapshot", &["--app", "demo", "--initialize"]));
    git(&index, &["init", "--initial-branch=main"]);
    git(&index, &["config", "user.name", "Index test"]);
    git(&index, &["config", "user.email", "index@example.test"]);
    git(&index, &["add", "."]);
    git(&index, &["commit", "-m", "Initial online roles"]);
    for name in ["snapshot.json", "timestamp.json"] {
        fs::remove_file(index.join("apps/demo").join(name)).unwrap();
    }
    git(&index, &["add", "."]);
    git(&index, &["commit", "-m", "Deleted online roles"]);
    targets(&index, &key, 8);
    success(run("snapshot", &["--app", "demo"]));
    for name in ["snapshot.json", "timestamp.json"] {
        let doc: serde_json::Value = read_json(&index.join("apps/demo").join(name)).unwrap();
        assert_eq!(doc["signed"]["version"], 2);
    }
    let base = temp.path().join("base");
    fs::create_dir_all(base.join("apps/demo")).unwrap();
    for name in ["snapshot.json", "timestamp.json"] {
        fs::copy(
            index.join("apps/demo").join(name),
            base.join("apps/demo").join(name),
        )
        .unwrap();
        fs::remove_file(index.join("apps/demo").join(name)).unwrap();
    }
    success(run(
        "snapshot",
        &["--app", "demo", "--previous", base.to_str().unwrap()],
    ));
    let doc: Signed<SnapshotSigned> = read_json(&index.join("apps/demo/snapshot.json")).unwrap();
    assert_eq!(doc.signed.version, 3);
}

#[test]
fn template_and_test_fixture_pass_full_index_verification() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../index-template");
    success(cli(&root, "verify", &[]));
    success(cli(&root.join("test/fixtures/index"), "verify", &[]));
}

#[test]
fn snapshot_is_idempotent_and_refreshes_timestamp_with_independent_versions() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path();
    let key = fixture(index);
    assert_eq!(
        String::from_utf8(cli(index, "pending-snapshots", &[]).stdout)
            .unwrap()
            .trim(),
        "demo"
    );
    success(cli(index, "snapshot", &["--app", "demo"]));
    success(cli(index, "verify", &[]));
    let dir = index.join("apps/demo");
    let before: Vec<_> = ["snapshot.json", "timestamp.json"]
        .map(|name| fs::read(dir.join(name)).unwrap())
        .into();
    success(cli(index, "snapshot", &["--app", "demo"]));
    success(cli(index, "snapshot", &["--app", "demo"]));
    for (name, bytes) in ["snapshot.json", "timestamp.json"].iter().zip(before) {
        assert_eq!(fs::read(dir.join(name)).unwrap(), bytes);
    }
    assert!(cli(index, "pending-snapshots", &[]).stdout.is_empty());
    for expected in [2, 3] {
        success(cli(index, "timestamp", &["--all"]));
        let timestamp: Signed<TimestampSigned> = read_json(&dir.join("timestamp.json")).unwrap();
        assert_eq!(timestamp.signed.version, expected);
    }
    targets(index, &key, 8);
    rejected(cli(index, "verify", &[]), "snapshot -> targets");
    success(cli(index, "verify", &["--targets-only"]));
    success(cli(index, "snapshot", &["--app", "demo"]));
    let snapshot: Signed<SnapshotSigned> = read_json(&dir.join("snapshot.json")).unwrap();
    let timestamp: Signed<TimestampSigned> = read_json(&dir.join("timestamp.json")).unwrap();
    assert_eq!(snapshot.signed.version, 2);
    assert_eq!(timestamp.signed.version, 4);
    success(cli(index, "verify", &[]));
    let mut expiring = snapshot.signed;
    expiring.expires = now_unix() + 86400;
    write_json(
        &dir.join("snapshot.json"),
        &sign(expiring, &[&key]).unwrap(),
    )
    .unwrap();
    assert!(!cli(index, "pending-snapshots", &[]).stdout.is_empty());
    success(cli(index, "snapshot", &["--app", "demo"]));
    let snapshot: Signed<SnapshotSigned> = read_json(&dir.join("snapshot.json")).unwrap();
    assert_eq!(snapshot.signed.version, 3);
    success(cli(index, "verify", &[]));
}

#[test]
fn verify_rejects_stale_timestamp_and_each_canonical_chain_component() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path();
    let key = fixture(index);
    success(cli(index, "snapshot", &["--app", "demo"]));
    let dir = index.join("apps/demo");
    let original: Signed<SnapshotSigned> = read_json(&dir.join("snapshot.json")).unwrap();
    // Same version, different expiry/signature: the exact incident from production.
    let mut changed = original.signed.clone();
    changed.expires += 1;
    write_json(&dir.join("snapshot.json"), &sign(changed, &[&key]).unwrap()).unwrap();
    rejected(cli(index, "verify", &[]), "timestamp -> snapshot");
    write_json(&dir.join("snapshot.json"), &original).unwrap();
    let timestamp: Signed<TimestampSigned> = read_json(&dir.join("timestamp.json")).unwrap();
    for component in ["version", "length", "hash", "reference", "signature"] {
        let mut broken = timestamp.signed.clone();
        let description = broken.meta.get_mut("snapshot.json").unwrap();
        match component {
            "version" => description.version += 1,
            "length" => description.length += 1,
            "hash" => {
                description.hashes.insert("sha256".into(), "00".repeat(32));
            }
            "reference" => {
                broken.meta.clear();
            }
            _ => {}
        }
        let mut doc = sign(broken, &[&key]).unwrap();
        if component == "signature" {
            doc.signatures.clear();
        }
        write_json(&dir.join("timestamp.json"), &doc).unwrap();
        rejected(
            cli(index, "verify", &[]),
            if component == "signature" {
                "timestamp signature"
            } else {
                "timestamp -> snapshot"
            },
        );
    }
    write_json(&dir.join("timestamp.json"), &timestamp).unwrap();
    let mut unsigned = original.clone();
    unsigned.signatures.clear();
    write_json(&dir.join("snapshot.json"), &unsigned).unwrap();
    rejected(cli(index, "verify", &[]), "snapshot signature");
    write_json(&dir.join("snapshot.json"), &original).unwrap();
    success(cli(index, "verify", &[]));
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    success(output);
}
fn bash_path(path: &Path) -> String {
    let path = path.to_str().unwrap().replace('\\', "/");
    let path = path.strip_prefix("//?/").unwrap_or(&path).to_string();
    if path.as_bytes().get(1) == Some(&b':') {
        format!("/{}{}", path[..1].to_lowercase(), &path[2..])
    } else {
        path
    }
}

#[test]
fn signing_push_conflict_resigns_latest_main_before_reporting_deployment_sha() {
    let bash = if cfg!(windows) {
        &format!(
            "{}/Git/bin/bash.exe",
            std::env::var("ProgramFiles").expect("ProgramFiles")
        )
    } else {
        "bash"
    };
    if Command::new(bash).arg("--version").output().is_err() {
        eprintln!("Git Bash is unavailable; skipping local signing push conflict test");
        return;
    }
    for mode in ["snapshot", "timestamp"] {
        let temp = tempfile::tempdir().unwrap();
        let seed = temp.path().join("seed");
        let key = fixture(&seed);
        success(cli(&seed, "snapshot", &["--app", "demo"]));
        targets(&seed, &key, 8);
        git(&seed, &["init", "--initial-branch=main"]);
        git(&seed, &["config", "user.name", "Index test"]);
        git(&seed, &["config", "user.email", "index@example.test"]);
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "Pending targets"]);
        let origin = temp.path().join("origin.git");
        git(
            temp.path(),
            &[
                "clone",
                "--bare",
                seed.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        let worker = temp.path().join("worker");
        let rival = temp.path().join("rival");
        for checkout in [&worker, &rival] {
            git(
                temp.path(),
                &[
                    "clone",
                    origin.to_str().unwrap(),
                    checkout.to_str().unwrap(),
                ],
            );
            git(checkout, &["config", "user.name", "Index test"]);
            git(checkout, &["config", "user.email", "index@example.test"]);
        }
        targets(&rival, &key, 9);
        let snapshot: Signed<SnapshotSigned> =
            read_json(&rival.join("apps/demo/snapshot.json")).unwrap();
        write_json(
            &rival.join("apps/demo/timestamp.json"),
            &build_timestamp(&snapshot, 80, expires_in(1), &key).unwrap(),
        )
        .unwrap();
        git(&rival, &["add", "."]);
        git(
            &rival,
            &["commit", "-m", "Concurrent targets and timestamp"],
        );
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(
            bin.join("vesper"),
            "#!/usr/bin/env bash\nexec \"$TEST_VESPER\" \"$@\"\n",
        )
        .unwrap();
        fs::write(
            bin.join("git"),
            r##"#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" push "* ]] && [ ! -f "$CONFLICT_MARKER" ]; then
  touch "$CONFLICT_MARKER"
  "$REAL_GIT" -C "$RIVAL" push origin HEAD:main
fi
exec "$REAL_GIT" "$@"
"##,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["git", "vesper"] {
                fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let git_location = Command::new(bash)
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(git_location.status.success());
        let output_file = temp.path().join("output.txt");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../index-template/scripts/sign-index.sh")
            .canonicalize()
            .unwrap();
        let result = Command::new(bash)
            .args([
                "-c",
                "export PATH=\"$TEST_BIN:$PATH\"; bash \"$SIGN_SCRIPT\" \"$SIGN_MODE\"",
            ])
            .current_dir(&worker)
            .env("TEST_BIN", bash_path(&bin))
            .env(
                "TEST_VESPER",
                bash_path(Path::new(env!("CARGO_BIN_EXE_vesper"))),
            )
            .env("SIGN_SCRIPT", bash_path(&script))
            .env("SIGN_MODE", mode)
            .env(
                "REAL_GIT",
                String::from_utf8(git_location.stdout).unwrap().trim(),
            )
            .env("RIVAL", bash_path(&rival))
            .env("CONFLICT_MARKER", bash_path(&temp.path().join("conflict")))
            .env("VESPER_ONLINE_KEY", SECRET)
            .env("RUNNER_TEMP", bash_path(temp.path()))
            .env("GITHUB_OUTPUT", bash_path(&output_file))
            .output()
            .unwrap();
        success(result);
        assert!(temp.path().join("conflict").exists());
        let snapshot: Signed<SnapshotSigned> =
            read_json(&worker.join("apps/demo/snapshot.json")).unwrap();
        let timestamp: Signed<TimestampSigned> =
            read_json(&worker.join("apps/demo/timestamp.json")).unwrap();
        assert_eq!(snapshot.signed.meta["targets.json"].version, 9);
        assert_eq!(snapshot.signed.version, 2);
        assert_eq!(
            timestamp.signed.version,
            if mode == "snapshot" { 81 } else { 82 }
        );
        success(cli(&worker, "verify", &[]));
        let head = Command::new("git")
            .arg("--git-dir")
            .arg(&origin)
            .args(["rev-parse", "main"])
            .output()
            .unwrap();
        assert!(
            head.status.success(),
            "read mock origin HEAD: {}",
            String::from_utf8_lossy(&head.stderr)
        );
        assert_eq!(
            fs::read_to_string(output_file).unwrap().trim(),
            format!("sha={}", String::from_utf8(head.stdout).unwrap().trim())
        );
    }
}

#[test]
fn aggregate_isolates_failed_app_reports_summary_and_initializes_registered_hosted_app() {
    use std::io::{Read, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index");
    let key = fixture(&index);
    success(cli(&index, "snapshot", &["--app", "demo"]));
    let bad = index.join("apps/bad");
    fs::create_dir_all(&bad).unwrap();
    for name in [
        "targets.json",
        "store.json",
        "description.md",
        "snapshot.json",
        "timestamp.json",
    ] {
        fs::copy(index.join("apps/demo").join(name), bad.join(name)).unwrap();
    }
    let old_bad = fs::read(bad.join("targets.json")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut routes: Signed<RoutesSigned> = read_json(&index.join("routes.json")).unwrap();
    let good_route = AppRoute {
        keys: vec![key.public_hex()],
        threshold: 1,
        metadata_url: Some(format!("{base}/good/")),
    };
    routes.signed.apps.insert("demo".into(), good_route.clone());
    routes
        .signed
        .apps
        .insert("fresh".into(), good_route.clone());
    routes.signed.apps.insert(
        "bad".into(),
        AppRoute {
            metadata_url: Some(format!("{base}/bad/")),
            ..good_route
        },
    );
    write_json(
        &index.join("routes.json"),
        &sign(routes.signed, &[&key]).unwrap(),
    )
    .unwrap();
    git(&index, &["init", "--initial-branch=main"]);
    git(&index, &["config", "user.name", "Index test"]);
    git(&index, &["config", "user.email", "index@example.test"]);
    git(&index, &["add", "."]);
    git(&index, &["commit", "-m", "Register developer hosting"]);
    let mut payload: Signed<AppTargetsSigned> =
        read_json(&index.join("apps/demo/targets.json")).unwrap();
    payload.signed.version = 8;
    let good = canonical_bytes(&sign(payload.signed.clone(), &[&key]).unwrap()).unwrap();
    let wrong = canonical_bytes(&sign(payload.signed, &[&KeyPair::generate()]).unwrap()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let worker = std::thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            };
            // Accepted sockets inherit non-blocking mode on Windows.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 8192];
            let Ok(n) = stream.read(&mut request) else {
                continue;
            };
            let text = String::from_utf8_lossy(&request[..n]);
            let path = text.split_whitespace().nth(1).unwrap_or("");
            let bytes: &[u8] = if path == "/good/targets.json" {
                &good
            } else if path == "/bad/targets.json" {
                &wrong
            } else {
                b"{}"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(bytes);
        }
    });
    let summary = temp.path().join("summary.md");
    let output = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(["index", "aggregate", "--index", index.to_str().unwrap()])
        .env("GITHUB_STEP_SUMMARY", &summary)
        .output()
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("bad: rejected:"), "{report}");
    assert!(report.contains("demo: updated"), "{report}");
    assert!(report.contains("fresh: updated"), "{report}");
    assert!(fs::read_to_string(summary)
        .unwrap()
        .contains("bad: rejected:"));
    success(output);
    assert_eq!(old_bad, fs::read(bad.join("targets.json")).unwrap());
    success(
        Command::new(env!("CARGO_BIN_EXE_vesper"))
            .args([
                "index",
                "snapshot",
                "--index",
                index.to_str().unwrap(),
                "--app",
                "fresh",
            ])
            .env("VESPER_ONLINE_KEY", SECRET)
            .output()
            .unwrap(),
    );
    let snapshot: Signed<SnapshotSigned> =
        read_json(&index.join("apps/fresh/snapshot.json")).unwrap();
    assert_eq!(snapshot.signed.version, 1);
    assert_eq!(snapshot.signed.meta["targets.json"].version, 8);
}
