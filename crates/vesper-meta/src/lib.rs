//! A small TUF-compatible trust model for the VESPER control plane.
//!
//! The wire documents use TUF role names and monotonic versions.  The signing
//! implementation intentionally keeps the trusted payloads explicit so a
//! client can reject unknown fields before installing content.

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod hosting;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureEntry {
    pub keyid: String,
    pub sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signed<T> {
    pub signed: T,
    pub signatures: Vec<SignatureEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleSpec {
    pub keyids: Vec<String>,
    pub threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub spec_version: String,
    pub version: u64,
    pub expires: u64,
    pub keys: BTreeMap<String, String>,
    pub roles: BTreeMap<String, RoleSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetDescription {
    pub length: u64,
    pub hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub custom: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub spec_version: String,
    pub version: u64,
    pub expires: u64,
    pub targets: BTreeMap<String, TargetDescription>,
    #[serde(default)]
    pub delegations: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaDescription {
    pub version: u64,
    pub length: u64,
    pub hashes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub spec_version: String,
    pub version: u64,
    pub expires: u64,
    pub meta: BTreeMap<String, MetaDescription>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimestampSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub spec_version: String,
    pub version: u64,
    pub expires: u64,
    pub meta: BTreeMap<String, MetaDescription>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRoute {
    pub keys: Vec<String>,
    pub threshold: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutesSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub spec_version: String,
    pub version: u64,
    pub expires: u64,
    pub apps: BTreeMap<String, AppRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppChannelManifest {
    pub release_tag: String,
    pub asset_name: String,
    pub size: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppChannel {
    pub version: u64,
    pub platform: String,
    pub manifest: AppChannelManifest,
    pub encrypted: bool,
    pub key_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSource {
    pub id: String,
    pub kind: String,
    pub url_template: String,
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppTargetsSigned {
    #[serde(rename = "_type")]
    pub typ: String,
    pub spec_version: String,
    pub version: u64,
    pub expires: u64,
    pub channels: BTreeMap<String, AppChannel>,
    pub sources: Vec<AppSource>,
    pub metadata: BTreeMap<String, StoreFileDescription>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreFileDescription {
    pub length: u64,
    pub sha256: String,
}

pub fn store_file_description(bytes: &[u8]) -> StoreFileDescription {
    StoreFileDescription {
        length: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
    }
}

pub fn store_metadata(dir: &std::path::Path) -> Result<BTreeMap<String, StoreFileDescription>> {
    ["store.json", "description.md"]
        .into_iter()
        .map(|name| {
            let bytes = std::fs::read(dir.join(name))
                .with_context(|| format!("read signed store file {name}"))?;
            Ok((name.into(), store_file_description(&bytes)))
        })
        .collect()
}

pub fn verify_store_bytes(targets: &AppTargetsSigned, name: &str, bytes: &[u8]) -> Result<()> {
    let expected = targets
        .metadata
        .get(name)
        .with_context(|| format!("missing signed store metadata for {name}"))?;
    let actual = store_file_description(bytes);
    if expected.length != actual.length || expected.sha256 != actual.sha256 {
        anyhow::bail!("signed store metadata length/SHA-256 mismatch for {name}");
    }
    Ok(())
}

pub fn verify_store_metadata(targets: &AppTargetsSigned, dir: &std::path::Path) -> Result<()> {
    for name in ["store.json", "description.md"] {
        let bytes = std::fs::read(dir.join(name))
            .with_context(|| format!("read signed store file {name}"))?;
        verify_store_bytes(targets, name, &bytes)?;
    }
    Ok(())
}

pub fn validate_url_template(template: &str) -> Result<()> {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let tail = &rest[start..];
        let end = tail
            .find('}')
            .context("URL template has an unterminated variable")?;
        let name = &tail[1..end];
        if name != "release" && name != "pack_id" {
            anyhow::bail!("URL template contains unsupported variable {{{name}}}");
        }
        rest = &tail[end + 1..];
    }
    Ok(())
}

pub fn validate_app_targets(targets: &AppTargetsSigned) -> Result<()> {
    if targets.typ != "targets" || targets.sources.is_empty() {
        anyhow::bail!("invalid application targets metadata");
    }
    if targets.metadata.len() != 2
        || ["store.json", "description.md"].iter().any(|name| {
            targets.metadata.get(*name).is_none_or(|value| {
                value.sha256.len() != 64 || !value.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            })
        })
    {
        anyhow::bail!("targets must sign store.json and description.md length/SHA-256 metadata");
    }
    for source in &targets.sources {
        if !matches!(source.kind.as_str(), "github" | "http" | "hf" | "r2") {
            anyhow::bail!("unsupported source kind {}", source.kind);
        }
        validate_url_template(&source.url_template)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct KeyPair {
    pub keyid: String,
    pub signing: SigningKey,
}

impl KeyPair {
    pub fn generate() -> Self {
        let signing = SigningKey::generate(&mut OsRng);
        let keyid = key_id(&signing.verifying_key());
        Self { keyid, signing }
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    pub fn from_secret_hex(value: &str) -> Result<Self> {
        let bytes = hex::decode(value.trim()).context("decode private key")?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("private key must be 32 bytes"))?;
        let signing = SigningKey::from_bytes(&bytes);
        Ok(Self {
            keyid: key_id(&signing.verifying_key()),
            signing,
        })
    }
}

/// Validate an Ed25519 public key and return its metadata key identifier.
pub fn public_key_id(bytes: &[u8]) -> Result<String> {
    let bytes: [u8; 32] = bytes.try_into().context("public key must be 32 bytes")?;
    Ok(key_id(&VerifyingKey::from_bytes(&bytes)?))
}

pub fn key_id(key: &VerifyingKey) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("serialize signed payload")
}

pub fn metadata_description<T: Serialize>(value: &T, version: u64) -> Result<MetaDescription> {
    let bytes = canonical_bytes(value)?;
    let mut hashes = BTreeMap::new();
    hashes.insert("sha256".to_string(), hex::encode(Sha256::digest(&bytes)));
    Ok(MetaDescription {
        version,
        length: bytes.len() as u64,
        hashes,
    })
}

pub fn build_snapshot(
    targets: &Signed<TargetsSigned>,
    version: u64,
    expires: u64,
    signer: &KeyPair,
) -> Result<Signed<SnapshotSigned>> {
    let mut meta = BTreeMap::new();
    meta.insert(
        "targets.json".to_string(),
        metadata_description(targets, targets.signed.version)?,
    );
    let payload = SnapshotSigned {
        typ: "snapshot".into(),
        spec_version: "1.0.0".into(),
        version,
        expires,
        meta,
    };
    sign(payload, &[signer])
}

pub fn build_timestamp(
    snapshot: &Signed<SnapshotSigned>,
    version: u64,
    expires: u64,
    signer: &KeyPair,
) -> Result<Signed<TimestampSigned>> {
    let mut meta = BTreeMap::new();
    meta.insert(
        "snapshot.json".to_string(),
        metadata_description(snapshot, snapshot.signed.version)?,
    );
    let payload = TimestampSigned {
        typ: "timestamp".into(),
        spec_version: "1.0.0".into(),
        version,
        expires,
        meta,
    };
    sign(payload, &[signer])
}

pub fn sign<T: Serialize>(payload: T, keys: &[&KeyPair]) -> Result<Signed<T>> {
    let bytes = canonical_bytes(&payload)?;
    let signatures = keys
        .iter()
        .map(|key| SignatureEntry {
            keyid: key.keyid.clone(),
            sig: hex::encode(key.signing.sign(&bytes).to_bytes()),
        })
        .collect();
    Ok(Signed {
        signed: payload,
        signatures,
    })
}

pub fn verify<T: Serialize>(
    document: &Signed<T>,
    keys: &BTreeMap<String, String>,
    threshold: u32,
) -> Result<()> {
    let bytes = canonical_bytes(&document.signed)?;
    let mut valid = std::collections::BTreeSet::new();
    for entry in &document.signatures {
        if valid.contains(&entry.keyid) {
            continue;
        }
        let Some(public_hex) = keys.get(&entry.keyid) else {
            continue;
        };
        let public = hex::decode(public_hex).context("decode public key")?;
        let public: [u8; 32] = public
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid public key length"))?;
        let verifying = VerifyingKey::from_bytes(&public).context("parse public key")?;
        let sig = hex::decode(&entry.sig).context("decode signature")?;
        let sig: [u8; 64] = sig
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid signature length"))?;
        verifying
            .verify(&bytes, &Signature::from_bytes(&sig))
            .context("signature verification failed")?;
        valid.insert(entry.keyid.clone());
    }
    if valid.len() < threshold as usize {
        anyhow::bail!("signature threshold not met: {}/{}", valid.len(), threshold);
    }
    Ok(())
}

pub fn verify_role<T: Serialize>(
    document: &Signed<T>,
    keys: &BTreeMap<String, String>,
    role: &RoleSpec,
) -> Result<()> {
    let allowed: BTreeMap<String, String> = role
        .keyids
        .iter()
        .filter_map(|id| keys.get(id).map(|key| (id.clone(), key.clone())))
        .collect();
    verify(document, &allowed, role.threshold)
}

/// File name of a versioned root published next to `root.json`, e.g. `2.root.json`.
pub fn versioned_root_name(version: u64) -> String {
    format!("{version}.root.json")
}

fn root_role(root: &RootSigned) -> Result<&RoleSpec> {
    anyhow::ensure!(root.typ == "root", "incorrect root role type");
    let role = root.roles.get("root").context("root role missing")?;
    anyhow::ensure!(
        role.threshold >= 1 && role.keyids.len() >= role.threshold as usize,
        "root role threshold must be between 1 and the number of root keys"
    );
    Ok(role)
}

/// Verify that a root document is signed by a threshold of its own root keys.
pub fn verify_root_self(root: &Signed<RootSigned>) -> Result<()> {
    verify_role(root, &root.signed.keys, root_role(&root.signed)?)
        .context("root is not signed by a threshold of its own root keys")
}

/// Verify `next` as the direct successor of an already trusted root (TUF root
/// chaining): its version must be exactly one higher, and it must be signed by
/// a threshold of the previous root's keys and by a threshold of its own.
/// Expiry is not checked here; intermediate roots in a chain may have expired.
pub fn verify_root_successor(
    trusted: &Signed<RootSigned>,
    next: &Signed<RootSigned>,
) -> Result<()> {
    let expected = trusted
        .signed
        .version
        .checked_add(1)
        .context("root version overflow")?;
    if next.signed.version != expected {
        anyhow::bail!(
            "root version rollback or gap: expected version {expected}, got {}",
            next.signed.version
        );
    }
    verify_role(next, &trusted.signed.keys, root_role(&trusted.signed)?).with_context(|| {
        format!(
            "root version {expected} is not signed by a threshold of the version {} root keys",
            trusted.signed.version
        )
    })?;
    verify_root_self(next).with_context(|| format!("root version {expected}"))
}

/// Whether the snapshot or timestamp signing keys differ between two roots.
pub fn online_roles_changed(previous: &RootSigned, next: &RootSigned) -> bool {
    ["snapshot", "timestamp"].iter().any(|name| {
        let keys = |root: &RootSigned| {
            root.roles.get(*name).map(|role| {
                let mut ids = role
                    .keyids
                    .iter()
                    .map(|id| (id.clone(), root.keys.get(id).cloned()))
                    .collect::<Vec<_>>();
                ids.sort();
                (ids, role.threshold)
            })
        };
        keys(previous) != keys(next)
    })
}

pub fn target_for_bytes(
    bytes: &[u8],
    custom: BTreeMap<String, serde_json::Value>,
) -> TargetDescription {
    let mut hashes = BTreeMap::new();
    hashes.insert("sha256".to_string(), hex::encode(Sha256::digest(bytes)));
    TargetDescription {
        length: bytes.len() as u64,
        hashes,
        custom,
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn expires_in(days: u64) -> u64 {
    now_unix().saturating_add(days.saturating_mul(24 * 60 * 60))
}

pub fn validate_version_and_expiry(
    version: u64,
    expires: u64,
    now: u64,
    previous_version: Option<u64>,
) -> Result<()> {
    if let Some(previous) = previous_version {
        if version < previous {
            anyhow::bail!("metadata version rollback: {} < {}", version, previous);
        }
    }
    if expires <= now {
        anyhow::bail!("metadata expired at {}", expires);
    }
    Ok(())
}

pub fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    let bytes = canonical_bytes(value)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).context("parse metadata JSON")
}

/// Store a developer seed in the Windows Credential Manager. CI callers use
/// `VESPER_DEV_KEY` instead; the seed is never written to a project file.
pub fn store_developer_seed(app_id: &str, seed_hex: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        };
        let target = format!("Vesper/{app_id}/developer");
        let target_w = std::ffi::OsStr::new(&target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut blob = seed_hex.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target_w.as_ptr() as *mut _,
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        if ok == 0 {
            anyhow::bail!("Windows Credential Manager rejected the developer key");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (app_id, seed_hex);
    }
    Ok(())
}

/// Read a developer seed from Windows Credential Manager, or from the CI
/// environment. The returned value is only held in memory by the caller.
pub fn load_developer_seed(app_id: &str) -> Result<String> {
    if let Ok(value) = std::env::var("VESPER_DEV_KEY") {
        return Ok(value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Credentials::{
            CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        };
        let target = format!("Vesper/{app_id}/developer");
        let target_w = std::ffi::OsStr::new(&target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
        if ok == 0 || credential.is_null() {
            anyhow::bail!("developer key is not configured for {app_id}");
        }
        let value = unsafe {
            let c = &*credential;
            let bytes = std::slice::from_raw_parts(
                c.CredentialBlob as *const u8,
                c.CredentialBlobSize as usize,
            );
            String::from_utf8(bytes.to_vec()).context("developer credential is not UTF-8")
        };
        unsafe {
            CredFree(credential as *mut _);
        }
        value
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("VESPER_DEV_KEY is required outside Windows for {app_id}")
    }
}

pub fn delete_developer_seed(app_id: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
        let target = format!("Vesper/{app_id}/developer");
        let target_w = std::ffi::OsStr::new(&target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let ok = unsafe { CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok == 0 {
            anyhow::bail!("failed to delete developer credential");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = app_id;
    }
    Ok(())
}

pub fn metadata_hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(&canonical_bytes(value)?)))
}

/// Verify the canonical signed documents used by both index tooling and clients.
pub fn verify_app_chain(
    targets: &Signed<AppTargetsSigned>,
    snapshot: &Signed<SnapshotSigned>,
    timestamp: &Signed<TimestampSigned>,
    root: &Signed<RootSigned>,
) -> Result<()> {
    for (role, version, expires) in [
        ("snapshot", snapshot.signed.version, snapshot.signed.expires),
        (
            "timestamp",
            timestamp.signed.version,
            timestamp.signed.expires,
        ),
    ] {
        validate_version_and_expiry(version, expires, now_unix(), None)
            .with_context(|| format!("{role} version/expiry"))?;
    }
    if snapshot.signed.typ != "snapshot" || timestamp.signed.typ != "timestamp" {
        anyhow::bail!("incorrect snapshot/timestamp role type");
    }
    if let Some(role) = root.signed.roles.get("snapshot") {
        verify_role(snapshot, &root.signed.keys, role).context("snapshot signature")?;
    }
    if let Some(role) = root.signed.roles.get("timestamp") {
        verify_role(timestamp, &root.signed.keys, role).context("timestamp signature")?;
    }
    verify_canonical_description(
        targets,
        targets.signed.version,
        snapshot
            .signed
            .meta
            .get("targets.json")
            .context("snapshot -> targets: missing reference")?,
    )
    .context("snapshot -> targets")?;
    verify_canonical_description(
        snapshot,
        snapshot.signed.version,
        timestamp
            .signed
            .meta
            .get("snapshot.json")
            .context("timestamp -> snapshot: missing reference")?,
    )
    .context("timestamp -> snapshot")?;
    Ok(())
}

pub fn verify_canonical_description<T: Serialize>(
    document: &T,
    version: u64,
    description: &MetaDescription,
) -> Result<()> {
    let actual = metadata_description(document, version)?;
    if description.version != actual.version
        || description.length != actual.length
        || description.hashes.get("sha256") != actual.hashes.get("sha256")
    {
        anyhow::bail!("metadata version, length or sha256 mismatch");
    }
    Ok(())
}

pub fn verify_metadata_description(
    path: &std::path::Path,
    description: &MetaDescription,
) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read metadata {}", path.display()))?;
    if bytes.len() as u64 != description.length {
        anyhow::bail!("metadata length mismatch for {}", path.display());
    }
    let expected = description
        .hashes
        .get("sha256")
        .context("metadata sha256 hash missing")?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if &actual != expected {
        anyhow::bail!("metadata hash mismatch for {}", path.display());
    }
    Ok(())
}

pub fn verify_routes(routes: &Signed<RoutesSigned>, root: &Signed<RootSigned>) -> Result<()> {
    anyhow::ensure!(routes.signed.typ == "routes", "incorrect routes role type");
    for (id, route) in &routes.signed.apps {
        hosting::validate_app_id(id)?;
        if let Some(url) = &route.metadata_url {
            hosting::metadata_base(url)?;
        }
    }
    let role = root.signed.roles.get("root").context("root role missing")?;
    verify_role(routes, &root.signed.keys, role)?;
    validate_version_and_expiry(
        routes.signed.version,
        routes.signed.expires,
        now_unix(),
        None,
    )
}

pub fn verify_app_targets(
    targets: &Signed<AppTargetsSigned>,
    route: &AppRoute,
    previous_version: Option<u64>,
) -> Result<()> {
    validate_app_targets(&targets.signed)?;
    // AppRoute stores public keys, while RoleSpec stores key ids. Accept
    // either representation so the on-disk format remains easy to bootstrap.
    let mut by_id = BTreeMap::new();
    for key in &route.keys {
        let public = key.clone();
        let decoded = hex::decode(&public).context("decode route public key")?;
        let public_bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!("route key must be 32-byte Ed25519 public key"))?;
        let id = key_id(&VerifyingKey::from_bytes(&public_bytes)?);
        by_id.insert(id, public);
    }
    let threshold = route.threshold.max(1);
    let bytes = canonical_bytes(&targets.signed)?;
    let mut valid = std::collections::BTreeSet::new();
    for signature in &targets.signatures {
        let Some(public_hex) = by_id.get(&signature.keyid) else {
            continue;
        };
        let public: [u8; 32] = hex::decode(public_hex)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid route public key"))?;
        let verifying = VerifyingKey::from_bytes(&public)?;
        let sig: [u8; 64] = hex::decode(&signature.sig)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid target signature"))?;
        verifying.verify(&bytes, &Signature::from_bytes(&sig))?;
        valid.insert(signature.keyid.clone());
    }
    if valid.len() < threshold as usize {
        anyhow::bail!(
            "application signature threshold not met: {}/{}",
            valid.len(),
            threshold
        );
    }
    validate_version_and_expiry(
        targets.signed.version,
        targets.signed.expires,
        now_unix(),
        previous_version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_signatures_are_verified() {
        let first = KeyPair::generate();
        let second = KeyPair::generate();
        let mut keys = BTreeMap::new();
        keys.insert(first.keyid.clone(), first.public_hex());
        keys.insert(second.keyid.clone(), second.public_hex());
        let payload = TargetsSigned {
            typ: "targets".into(),
            spec_version: "1.0.0".into(),
            version: 1,
            expires: expires_in(1),
            targets: BTreeMap::new(),
            delegations: None,
        };
        let signed = sign(payload, &[&first, &second]).unwrap();
        verify(&signed, &keys, 2).unwrap();
    }

    #[test]
    fn snapshot_and_timestamp_link_signed_documents() {
        let key = KeyPair::generate();
        let targets = sign(
            TargetsSigned {
                typ: "targets".into(),
                spec_version: "1.0.0".into(),
                version: 4,
                expires: expires_in(90),
                targets: BTreeMap::new(),
                delegations: None,
            },
            &[&key],
        )
        .unwrap();
        let snapshot = build_snapshot(&targets, 8, expires_in(7), &key).unwrap();
        let timestamp = build_timestamp(&snapshot, 9, expires_in(1), &key).unwrap();
        let keys = BTreeMap::from([(key.keyid.clone(), key.public_hex())]);
        verify(&snapshot, &keys, 1).unwrap();
        verify(&timestamp, &keys, 1).unwrap();
        assert_eq!(snapshot.signed.meta["targets.json"].version, 4);
        assert_eq!(timestamp.signed.meta["snapshot.json"].version, 8);
    }

    #[test]
    fn expired_or_rollback_metadata_is_rejected() {
        assert!(validate_version_and_expiry(1, 10, 10, None).is_err());
        assert!(validate_version_and_expiry(1, 20, 0, Some(2)).is_err());
    }

    #[test]
    fn per_application_route_signature_is_verified() {
        let key = KeyPair::generate();
        let route = AppRoute {
            keys: vec![key.public_hex()],
            threshold: 1,
            metadata_url: None,
        };
        let payload = AppTargetsSigned {
            metadata: ["store.json", "description.md"]
                .into_iter()
                .map(|name| (name.into(), store_file_description(b"{}")))
                .collect(),
            typ: "targets".into(),
            spec_version: "1.0.0".into(),
            version: 1,
            expires: expires_in(1),
            channels: BTreeMap::new(),
            sources: vec![AppSource {
                id: "http".into(),
                kind: "http".into(),
                url_template: "{release}/{pack_id}".into(),
                priority: 0,
            }],
        };
        let signed = sign(payload, &[&key]).unwrap();
        verify_app_targets(&signed, &route, None).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn credential_manager_round_trip_uses_a_test_only_target() {
        let app = format!("test-only-{}", std::process::id());
        let seed = "11".repeat(32);
        store_developer_seed(&app, &seed).unwrap();
        let loaded = load_developer_seed(&app).unwrap();
        assert_eq!(loaded, seed);
        delete_developer_seed(&app).unwrap();
    }
}
