//! Browser side of Vesper: the same metadata and content checks as the desktop
//! client, compiled to WebAssembly for the web store and the in-browser runtime.
//!
//! JavaScript does all networking and storage. It hands raw bytes to these
//! functions, which verify the signed catalogue chain, open a manifest whose
//! size and BLAKE3 hash come from verified targets, plan the pack frames that
//! cover a byte range of a file, and decode and verify each frame. Results are
//! returned as JSON strings so the JavaScript side needs no generated types.
//!
//! The `*_impl` functions hold the logic and are tested natively; the exported
//! wrappers only convert errors for JavaScript.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use vesper_format::{ChunkId, Manifest, PackFrame, MAX_FRAME_RAW_SIZE};
use vesper_meta::{
    AppTargetsSigned, RootSigned, RoutesSigned, Signed, SnapshotSigned, TimestampSigned,
};
use wasm_bindgen::prelude::*;

/// Package platform identity of browser builds.
pub const WEB_PLATFORM: &str = "web-wasm32";

fn js(result: Result<impl Into<JsValue>>) -> Result<JsValue, JsError> {
    result
        .map(Into::into)
        .map_err(|error| JsError::new(&format!("{error:#}")))
}

fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(bytes).with_context(|| format!("parse {what}"))
}

/// Check that a trust anchor is signed by a threshold of its own root keys and
/// return its version.
pub fn root_anchor_impl(anchor: &[u8]) -> Result<u64> {
    let root: Signed<RootSigned> = parse(anchor, "trusted root")?;
    vesper_meta::verify_root_self(&root).context("trusted root")?;
    Ok(root.signed.version)
}

/// Verify `next` as the direct successor of `trusted` and report whether it
/// replaces the online snapshot/timestamp keys (cached online versions must
/// then be forgotten).
pub fn root_successor_impl(trusted: &[u8], next: &[u8]) -> Result<String> {
    let trusted: Signed<RootSigned> = parse(trusted, "trusted root")?;
    let next: Signed<RootSigned> = parse(next, "next root")?;
    vesper_meta::verify_root_successor(&trusted, &next)?;
    Ok(json!({
        "version": next.signed.version,
        "online_roles_changed": vesper_meta::online_roles_changed(&trusted.signed, &next.signed),
    })
    .to_string())
}

/// After chaining, the newest verified root must be unexpired and equal to the
/// index's `root.json`, which rejects a rolled-back `root.json`.
pub fn root_current_impl(trusted: &[u8], published: &[u8], now: u64) -> Result<()> {
    let trusted: Signed<RootSigned> = parse(trusted, "trusted root")?;
    let published: Signed<RootSigned> = parse(published, "root.json")?;
    anyhow::ensure!(
        trusted.signed.expires > now,
        "catalogue root version {} expired; the index has not published a renewed root",
        trusted.signed.version
    );
    anyhow::ensure!(
        serde_json::to_value(&published)? == serde_json::to_value(&trusted)?,
        "index root.json (version {}) does not match the verified root chain (version {})",
        published.signed.version,
        trusted.signed.version
    );
    Ok(())
}

/// Verify `routes.json` against a verified root and return the application ids.
pub fn verify_routes_impl(root: &[u8], routes: &[u8]) -> Result<String> {
    let root: Signed<RootSigned> = parse(root, "root")?;
    let routes: Signed<RoutesSigned> = parse(routes, "routes.json")?;
    vesper_meta::verify_routes(&routes, &root)?;
    Ok(serde_json::to_string(
        &routes.signed.apps.keys().collect::<Vec<_>>(),
    )?)
}

/// Versions this browser has already accepted for an application. Lower
/// versions are rejected as rollbacks.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SeenVersions {
    /// Highest accepted targets version.
    pub targets: Option<u64>,
    /// Highest accepted snapshot version.
    pub snapshot: Option<u64>,
    /// Highest accepted timestamp version.
    pub timestamp: Option<u64>,
}

/// Raw files of one application as served by the catalogue.
pub struct AppFiles<'a> {
    /// `apps/<id>/targets.json`
    pub targets: &'a [u8],
    /// `apps/<id>/snapshot.json`
    pub snapshot: &'a [u8],
    /// `apps/<id>/timestamp.json`
    pub timestamp: &'a [u8],
    /// `apps/<id>/store.json`
    pub store: &'a [u8],
    /// `apps/<id>/description.md`
    pub description: &'a [u8],
}

/// Verify one application against a verified root and routes: developer
/// signatures on targets, the online snapshot/timestamp chain, expiry, version
/// monotonicity against `seen`, and the exact store and description bytes.
/// Returns the verified targets payload, store, description and new versions.
pub fn verify_app_impl(
    root: &[u8],
    routes: &[u8],
    app_id: &str,
    files: AppFiles<'_>,
    seen: &SeenVersions,
) -> Result<String> {
    vesper_meta::hosting::validate_app_id(app_id)?;
    let root: Signed<RootSigned> = parse(root, "root")?;
    let routes: Signed<RoutesSigned> = parse(routes, "routes.json")?;
    vesper_meta::verify_routes(&routes, &root)?;
    let route = routes
        .signed
        .apps
        .get(app_id)
        .context("application is not present in routes.json")?;
    let targets: Signed<AppTargetsSigned> = parse(files.targets, "targets.json")?;
    vesper_meta::verify_app_targets(&targets, route, seen.targets)?;
    let snapshot: Signed<SnapshotSigned> = parse(files.snapshot, "snapshot.json")?;
    let timestamp: Signed<TimestampSigned> = parse(files.timestamp, "timestamp.json")?;
    let now = vesper_meta::now_unix();
    vesper_meta::validate_version_and_expiry(
        snapshot.signed.version,
        snapshot.signed.expires,
        now,
        seen.snapshot,
    )
    .context("snapshot")?;
    vesper_meta::validate_version_and_expiry(
        timestamp.signed.version,
        timestamp.signed.expires,
        now,
        seen.timestamp,
    )
    .context("timestamp")?;
    // Browsers never accept a bootstrap index without online roles.
    for role in ["snapshot", "timestamp"] {
        anyhow::ensure!(
            root.signed.roles.contains_key(role),
            "trusted index root is missing required {role} role"
        );
    }
    vesper_meta::verify_app_chain(&targets, &snapshot, &timestamp, &root)?;
    vesper_meta::verify_store_bytes(&targets.signed, "store.json", files.store)?;
    vesper_meta::verify_store_bytes(&targets.signed, "description.md", files.description)?;
    let store: Value = parse(files.store, "store.json")?;
    anyhow::ensure!(store.is_object(), "store.json must be an object");
    let description =
        std::str::from_utf8(files.description).context("description.md must be UTF-8")?;
    Ok(json!({
        "app_id": app_id,
        "targets": targets.signed,
        "store": store,
        "description": description,
        "seen": SeenVersions {
            targets: Some(targets.signed.version),
            snapshot: Some(snapshot.signed.version),
            timestamp: Some(timestamp.signed.version),
        },
    })
    .to_string())
}

/// Choose the channel a browser plays for `requested`: `<requested>-web-wasm32`
/// first, then `<requested>` itself when its build is a web build.
pub fn web_channel_impl(targets: &[u8], requested: &str) -> Result<Option<String>> {
    let targets: AppTargetsSigned = parse(targets, "targets payload")?;
    let suffixed = format!("{requested}-{WEB_PLATFORM}");
    for name in [suffixed.as_str(), requested] {
        if targets
            .channels
            .get(name)
            .is_some_and(|channel| channel.platform == WEB_PLATFORM && !channel.encrypted)
        {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

/// A decoded, validated manifest whose bytes matched the signed size and hash.
#[wasm_bindgen]
pub struct WebManifest {
    manifest: Manifest,
    by_path: BTreeMap<String, usize>,
    packs: BTreeMap<String, Option<String>>,
}

impl WebManifest {
    /// Check the manifest bytes against the signed length and BLAKE3 hash, then
    /// decode and validate. Encrypted (paid) manifests are not supported yet.
    pub fn open_impl(bytes: &[u8], size: u64, blake3_hex: &str) -> Result<Self> {
        anyhow::ensure!(
            bytes.len() as u64 == size,
            "manifest length mismatch: expected {size}, got {}",
            bytes.len()
        );
        anyhow::ensure!(
            hex::encode(vesper_format::hash_bytes(bytes)).eq_ignore_ascii_case(blake3_hex),
            "manifest BLAKE3 mismatch"
        );
        anyhow::ensure!(
            !bytes.starts_with(vesper_format::MANIFEST_MAGIC),
            "encrypted builds cannot be played in the browser yet"
        );
        let manifest = Manifest::decode(bytes)?;
        anyhow::ensure!(
            manifest.platform == WEB_PLATFORM,
            "build platform is {}, not {WEB_PLATFORM}",
            manifest.platform
        );
        let by_path = manifest
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), index))
            .collect();
        let packs = manifest
            .packs
            .iter()
            .map(|pack| (pack.pack_id.clone(), pack.release_tag.clone()))
            .collect();
        Ok(Self {
            manifest,
            by_path,
            packs,
        })
    }

    /// JSON summary: identity, launch entry and every file with its size.
    pub fn summary_impl(&self) -> Result<String> {
        let m = &self.manifest;
        Ok(json!({
            "app_id": m.app_id,
            "platform": m.platform,
            "version": m.version,
            "launch": m.install.launch.as_ref().map(|l| &l.executable),
            "files": m.files.iter().map(|f| json!({"path": f.path, "size": f.size})).collect::<Vec<_>>(),
        })
        .to_string())
    }

    /// Pack frames covering bytes `start..end` of `path` (end is clamped to the
    /// file size). Each entry gives the chunk id, its pack and release, the
    /// frame location and where the chunk's raw bytes start in the file.
    pub fn plan_impl(&self, path: &str, start: u64, end: u64) -> Result<String> {
        let index = *self
            .by_path
            .get(path)
            .with_context(|| format!("file is not in this build: {path}"))?;
        let file = &self.manifest.files[index];
        let end = end.min(file.size);
        let mut offset = 0u64;
        let mut chunks = Vec::new();
        for chunk in &file.chunks {
            let chunk_end = offset + chunk.raw_size;
            if chunk_end > start && offset < end {
                let location = &self.manifest.chunks[&chunk.id];
                chunks.push(json!({
                    "id": chunk.id.hex(),
                    "pack": location.pack_id,
                    "release": self.packs.get(&location.pack_id).cloned().flatten(),
                    "offset": location.offset,
                    "frame_len": location.frame_len,
                    "raw_size": chunk.raw_size,
                    "file_offset": offset,
                }));
            }
            offset = chunk_end;
            if offset >= end {
                break;
            }
        }
        Ok(json!({ "size": file.size, "chunks": chunks }).to_string())
    }
}

#[wasm_bindgen]
impl WebManifest {
    /// See [`WebManifest::open_impl`].
    pub fn open(bytes: &[u8], size: u64, blake3_hex: &str) -> Result<WebManifest, JsError> {
        Self::open_impl(bytes, size, blake3_hex).map_err(|e| JsError::new(&format!("{e:#}")))
    }

    /// See [`WebManifest::summary_impl`].
    pub fn summary(&self) -> Result<JsValue, JsError> {
        js(self.summary_impl())
    }

    /// See [`WebManifest::plan_impl`].
    pub fn plan(&self, path: &str, start: u64, end: u64) -> Result<JsValue, JsError> {
        js(self.plan_impl(path, start, end))
    }
}

/// Decode one pack frame and verify it: the stored tag, the decompressed size
/// and that the raw bytes hash to the expected chunk id.
pub fn decode_frame_impl(frame: &[u8], chunk_id_hex: &str, raw_size: u64) -> Result<Vec<u8>> {
    let parsed = PackFrame::read_from(frame, MAX_FRAME_RAW_SIZE)?;
    anyhow::ensure!(
        parsed.encoded_len() == frame.len() as u64,
        "frame length does not match its location"
    );
    let raw = parsed.decode_payload(None)?;
    anyhow::ensure!(raw.len() as u64 == raw_size, "chunk raw size mismatch");
    anyhow::ensure!(
        ChunkId::from_bytes(&raw).hex() == chunk_id_hex,
        "chunk {chunk_id_hex} failed verification"
    );
    Ok(raw)
}

/// Decompress a Brotli file the way a web server with `Content-Encoding: br`
/// would, refusing output larger than `limit` bytes.
pub fn brotli_decompress_impl(bytes: &[u8], limit: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    brotli_decompressor::Decompressor::new(bytes, 64 * 1024)
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .context("brotli decompression failed")?;
    anyhow::ensure!(
        out.len() as u64 <= limit,
        "decompressed file exceeds {limit} bytes"
    );
    Ok(out)
}

/// Lowercase hex BLAKE3 of `bytes`, used to verify store media.
#[wasm_bindgen(js_name = blake3Hex)]
pub fn blake3_hex(bytes: &[u8]) -> String {
    hex::encode(vesper_format::hash_bytes(bytes))
}

/// See [`root_anchor_impl`].
#[wasm_bindgen(js_name = rootAnchor)]
pub fn root_anchor(anchor: &[u8]) -> Result<JsValue, JsError> {
    js(root_anchor_impl(anchor).map(|v| v as f64))
}

/// See [`root_successor_impl`].
#[wasm_bindgen(js_name = rootSuccessor)]
pub fn root_successor(trusted: &[u8], next: &[u8]) -> Result<JsValue, JsError> {
    js(root_successor_impl(trusted, next))
}

/// See [`root_current_impl`].
#[wasm_bindgen(js_name = rootCurrent)]
pub fn root_current(trusted: &[u8], published: &[u8]) -> Result<(), JsError> {
    root_current_impl(trusted, published, vesper_meta::now_unix())
        .map_err(|e| JsError::new(&format!("{e:#}")))
}

/// See [`verify_routes_impl`].
#[wasm_bindgen(js_name = verifyRoutes)]
pub fn verify_routes(root: &[u8], routes: &[u8]) -> Result<JsValue, JsError> {
    js(verify_routes_impl(root, routes))
}

/// See [`verify_app_impl`]. `seen` is the JSON returned by an earlier call, or
/// an empty string.
#[wasm_bindgen(js_name = verifyApp)]
#[allow(clippy::too_many_arguments)]
pub fn verify_app(
    root: &[u8],
    routes: &[u8],
    app_id: &str,
    targets: &[u8],
    snapshot: &[u8],
    timestamp: &[u8],
    store: &[u8],
    description: &[u8],
    seen: &str,
) -> Result<JsValue, JsError> {
    js((|| {
        let seen = if seen.is_empty() {
            SeenVersions::default()
        } else {
            serde_json::from_str(seen).context("parse seen versions")?
        };
        verify_app_impl(
            root,
            routes,
            app_id,
            AppFiles {
                targets,
                snapshot,
                timestamp,
                store,
                description,
            },
            &seen,
        )
    })())
}

/// Check an optional signed store document (`changelog.md`,
/// `description.<lang>.md`) against a verified targets payload and return its text.
pub fn verify_document_impl(targets_payload: &[u8], name: &str, bytes: &[u8]) -> Result<String> {
    anyhow::ensure!(
        vesper_meta::store::is_store_file_name(name) && name.ends_with(".md"),
        "{name} is not a store document"
    );
    let targets: AppTargetsSigned = parse(targets_payload, "targets payload")?;
    vesper_meta::verify_store_bytes(&targets, name, bytes)?;
    Ok(std::str::from_utf8(bytes)
        .with_context(|| format!("{name} must be UTF-8"))?
        .to_owned())
}

/// See [`verify_document_impl`].
#[wasm_bindgen(js_name = verifyDocument)]
pub fn verify_document(targets_payload: &str, name: &str, bytes: &[u8]) -> Result<String, JsError> {
    verify_document_impl(targets_payload.as_bytes(), name, bytes)
        .map_err(|e| JsError::new(&format!("{e:#}")))
}

/// See [`web_channel_impl`]. Returns `undefined` when no web build exists.
#[wasm_bindgen(js_name = webChannel)]
pub fn web_channel(targets_payload: &str, requested: &str) -> Result<Option<String>, JsError> {
    web_channel_impl(targets_payload.as_bytes(), requested)
        .map_err(|e| JsError::new(&format!("{e:#}")))
}

/// See [`decode_frame_impl`].
#[wasm_bindgen(js_name = decodeFrame)]
pub fn decode_frame(frame: &[u8], chunk_id_hex: &str, raw_size: u64) -> Result<Vec<u8>, JsError> {
    decode_frame_impl(frame, chunk_id_hex, raw_size).map_err(|e| JsError::new(&format!("{e:#}")))
}

/// See [`brotli_decompress_impl`].
#[wasm_bindgen(js_name = brotliDecompress)]
pub fn brotli_decompress(bytes: &[u8], limit: u64) -> Result<Vec<u8>, JsError> {
    brotli_decompress_impl(bytes, limit).map_err(|e| JsError::new(&format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use vesper_format::ChunkingParams;

    fn build(platform: &str) -> (tempfile::TempDir, vesper_format::BuildOutput) {
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        fs::write(input.path().join("index.html"), b"<!doctype html>hello").unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 31 % 251) as u8).collect();
        fs::create_dir_all(input.path().join("Build")).unwrap();
        fs::write(input.path().join("Build/game.data"), &big).unwrap();
        let built = vesper_format::build_from_directory(
            input.path(),
            output.path(),
            "demo.web".into(),
            platform.into(),
            "1".into(),
            ChunkingParams {
                min_size: 4096,
                avg_size: 16384,
                max_size: 65536,
            },
            1024 * 1024,
        )
        .unwrap();
        (output, built)
    }

    fn open(built: &vesper_format::BuildOutput) -> Result<WebManifest> {
        let bytes = fs::read(&built.manifest_path).unwrap();
        let hash = hex::encode(vesper_format::hash_bytes(&bytes));
        WebManifest::open_impl(&bytes, bytes.len() as u64, &hash)
    }

    #[test]
    fn plans_and_decodes_a_file_range() {
        let (_output, built) = build(WEB_PLATFORM);
        let manifest = open(&built).unwrap();
        let plan: Value = serde_json::from_str(
            &manifest
                .plan_impl("Build/game.data", 100_000, 100_010)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(plan["size"], 300_000);
        let chunks = plan["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), 1, "a ten-byte range lies in one chunk");
        let chunk = &chunks[0];
        let pack_path = built
            .packs
            .iter()
            .find(|p| p.file_name().unwrap() == chunk["pack"].as_str().unwrap())
            .unwrap();
        let pack = fs::read(pack_path).unwrap();
        let offset = chunk["offset"].as_u64().unwrap() as usize;
        let frame = &pack[offset..offset + chunk["frame_len"].as_u64().unwrap() as usize];
        let id = chunk["id"].as_str().unwrap();
        let raw = decode_frame_impl(frame, id, chunk["raw_size"].as_u64().unwrap()).unwrap();
        let start = chunk["file_offset"].as_u64().unwrap();
        assert!(start <= 100_000 && start + raw.len() as u64 >= 100_010);
        assert_eq!(
            raw[(100_000 - start) as usize],
            (100_000u32 * 31 % 251) as u8
        );

        let mut tampered = frame.to_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(decode_frame_impl(&tampered, id, raw.len() as u64).is_err());
        assert!(manifest.plan_impl("missing.txt", 0, 1).is_err());
    }

    #[test]
    fn manifest_must_match_signed_hash_and_web_platform() {
        let (_output, built) = build(WEB_PLATFORM);
        let bytes = fs::read(&built.manifest_path).unwrap();
        assert!(WebManifest::open_impl(&bytes, bytes.len() as u64, &"0".repeat(64)).is_err());
        assert!(WebManifest::open_impl(&bytes, 1, "").is_err());
        let summary: Value =
            serde_json::from_str(&open(&built).unwrap().summary_impl().unwrap()).unwrap();
        assert_eq!(summary["files"].as_array().unwrap().len(), 2);

        let (_output, desktop) = build("windows-x64");
        assert!(open(&desktop).is_err(), "desktop builds are not playable");
    }

    #[test]
    fn web_channel_prefers_the_platform_suffix() {
        let channel = |platform: &str| {
            json!({"version": 1, "platform": platform, "encrypted": false, "key_id": 0,
                "manifest": {"release_tag": "r", "asset_name": "manifest.cbor.zst", "size": 1, "blake3": ""}})
        };
        let targets = |channels: Value| {
            json!({"_type": "targets", "spec_version": "1.0.0", "version": 1, "expires": 1,
                "channels": channels, "sources": [], "metadata": {}})
            .to_string()
        };
        let both = targets(
            json!({"stable": channel("windows-x64"), "stable-web-wasm32": channel(WEB_PLATFORM)}),
        );
        assert_eq!(
            web_channel_impl(both.as_bytes(), "stable")
                .unwrap()
                .as_deref(),
            Some("stable-web-wasm32")
        );
        let plain = targets(json!({"stable": channel(WEB_PLATFORM)}));
        assert_eq!(
            web_channel_impl(plain.as_bytes(), "stable")
                .unwrap()
                .as_deref(),
            Some("stable")
        );
        let desktop = targets(json!({"stable": channel("windows-x64")}));
        assert_eq!(
            web_channel_impl(desktop.as_bytes(), "stable").unwrap(),
            None
        );
    }

    #[test]
    fn documents_must_match_their_signed_hash() {
        let text = b"## 0.2.0
- New vans
";
        let description = vesper_meta::store_file_description(text);
        let targets = json!({"_type": "targets", "spec_version": "1.0.0", "version": 1, "expires": 1,
            "channels": {}, "sources": [],
            "metadata": {"changelog.md": {"length": description.length, "sha256": description.sha256}}})
        .to_string();
        assert_eq!(
            verify_document_impl(targets.as_bytes(), "changelog.md", text).unwrap(),
            "## 0.2.0
- New vans
"
        );
        let changed = verify_document_impl(
            targets.as_bytes(),
            "changelog.md",
            b"## 0.2.0
- Old vans
",
        );
        assert!(changed.unwrap_err().to_string().contains("mismatch"));
        // Unsigned or non-document names are refused before any hashing.
        assert!(verify_document_impl(targets.as_bytes(), "changelog.ja.md", text).is_err());
        assert!(verify_document_impl(targets.as_bytes(), "store.json", text)
            .unwrap_err()
            .to_string()
            .contains("not a store document"));
    }

    #[test]
    fn brotli_output_is_bounded() {
        // A tiny valid Brotli stream: "hello" stored uncompressed (window 16).
        let stream = [0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03];
        assert_eq!(brotli_decompress_impl(&stream, 5).unwrap(), b"hello");
        assert!(brotli_decompress_impl(&stream, 4).is_err());
        assert!(brotli_decompress_impl(b"not brotli", 100).is_err());
    }
}
