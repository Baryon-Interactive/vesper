use crate::{
    http::{client, get_json, join_url},
    state::state_dir,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use vesper_fetch::{Source, SourceSpec};
use vesper_meta::{
    canonical_bytes, validate_version_and_expiry, verify_app_targets, verify_role, verify_routes,
    AppRoute, AppTargetsSigned, RootSigned, RoutesSigned, Signed, SnapshotSigned, TimestampSigned,
};

const BUILTIN_ROOT: &str = include_str!("../config/root.json");
const TRUSTED_ROOT_ENV: &str = "VESPER_TRUSTED_ROOT";

/// Verify and cache the signed metadata chain for an application.
pub fn verify_app(base: &str, app_id: &str) -> Result<(Signed<AppTargetsSigned>, AppRoute)> {
    vesper_meta::hosting::validate_app_id(app_id)?;
    fetch_targets(base, app_id)
}

static SEEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Prefer developer metadata for update/install operations. Route authority is
/// always obtained from the central root-signed registry. Report rejections and
/// fall back to the complete central chain without lowering the local high-water mark.
pub fn verify_update(base: &str, app_id: &str) -> Result<(Signed<AppTargetsSigned>, AppRoute)> {
    vesper_meta::hosting::validate_app_id(app_id)?;
    let _lock = SEEN_LOCK.lock().unwrap();
    let (_, routes) = metadata_root(base)?;
    let route = routes
        .signed
        .apps
        .get(app_id)
        .cloned()
        .context("application is not present in routes.json")?;
    let seen_path = state_dir()
        .join("metadata")
        .join(app_id)
        .join("highest-targets.json");
    let mut previous: Option<Signed<AppTargetsSigned>> = if seen_path.exists() {
        Some(serde_json::from_slice(&fs::read(&seen_path)?)?)
    } else {
        None
    };
    let central_path = seen_path.with_file_name("targets.json");
    if central_path.exists() {
        let central: Signed<AppTargetsSigned> = serde_json::from_slice(&fs::read(central_path)?)?;
        if previous
            .as_ref()
            .is_none_or(|p| central.signed.version > p.signed.version)
        {
            previous = Some(central);
        }
    }
    if let Some(url) = &route.metadata_url {
        match vesper_meta::hosting::Bundle::fetch(url, &route, previous.as_ref()) {
            Ok(bundle) => {
                cache_metadata(app_id, "highest-targets.json", &bundle.targets)?;
                eprintln!("{app_id}: update metadata source=developer version={}", bundle.targets.signed.version);
                return Ok((bundle.targets, route));
            }
            Err(error) => eprintln!("{app_id}: developer metadata rejected: {error:#}; falling back to central catalogue"),
        }
    }
    let (targets, route) = fetch_targets(base, app_id)?;
    let app_base = join_url(base, &format!("apps/{app_id}"));
    let store = targets
        .signed
        .metadata
        .keys()
        .map(|name| {
            Ok((
                name.clone(),
                vesper_meta::hosting::fetch_file(&app_base, name)?,
            ))
        })
        .collect::<Result<_>>()?;
    vesper_meta::hosting::Bundle {
        targets: targets.clone(),
        targets_bytes: canonical_bytes(&targets)?,
        store,
    }
    .verify(&route, previous.as_ref())?;
    cache_metadata(app_id, "highest-targets.json", &targets)?;
    eprintln!(
        "{app_id}: update metadata source=central version={}",
        targets.signed.version
    );
    Ok((targets, route))
}
/// Reject expired metadata and monotonic version rollback.
pub fn check_version(version: u64, expires: u64, now: u64, previous: Option<u64>) -> Result<()> {
    validate_version_and_expiry(version, expires, now, previous)
}

fn verify_online_role<T: Serialize>(
    document: &Signed<T>,
    root: &Signed<RootSigned>,
    role: &str,
) -> Result<()> {
    let trusted = trusted_root()?;
    verify_online_role_with_bootstrap(document, root, role, trusted.signed.keys.is_empty())
}

fn verify_online_role_with_bootstrap<T: Serialize>(
    document: &Signed<T>,
    root: &Signed<RootSigned>,
    role: &str,
    bootstrap: bool,
) -> Result<()> {
    if let Some(spec) = root.signed.roles.get(role) {
        verify_role(document, &root.signed.keys, spec)
    } else if bootstrap && cfg!(debug_assertions) {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| eprintln!("warning: demo bootstrap index lacks online signing roles; configure snapshot and timestamp roles before production"));
        Ok(())
    } else {
        anyhow::bail!("trusted index root is missing required {role} role")
    }
}

fn metadata_root(base: &str) -> Result<(Signed<RootSigned>, Signed<RoutesSigned>)> {
    let anchor = trusted_root()?;
    let root = {
        let _lock = ROOT_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        update_root(
            &anchor,
            &state_dir().join("trust"),
            &state_dir().join("metadata"),
            vesper_meta::now_unix(),
            |name| fetch_optional_json(&join_url(base, name)),
        )?
    };
    let routes: Signed<RoutesSigned> = get_json(&join_url(base, "routes.json"))?;
    verify_routes(&routes, &root)?;
    Ok((root, routes))
}

/// Serialises root updates so concurrent catalogue requests never interleave
/// cache writes.
static ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Upper bound on root versions followed in one update, as in TUF clients.
const MAX_ROOT_ROTATIONS: usize = 256;

/// Establish the current catalogue root by TUF root chaining.
///
/// Starts from the trust anchor (the embedded root or `VESPER_TRUSTED_ROOT`), or
/// from the newest root previously verified from that anchor, then follows
/// `N+1.root.json`, `N+2.root.json`, ... until the index has no further version.
/// Each successor must be exactly one version higher and signed by a threshold
/// of the previous root's keys and by a threshold of its own. Every verified
/// root is cached immediately. The final root must not be expired, and the
/// index's `root.json` must equal it, which rejects a rolled-back `root.json`.
///
/// `fetch` returns `Ok(None)` when the named file does not exist on the index.
fn update_root(
    anchor: &Signed<RootSigned>,
    trust_dir: &std::path::Path,
    metadata_dir: &std::path::Path,
    now: u64,
    mut fetch: impl FnMut(&str) -> Result<Option<Signed<RootSigned>>>,
) -> Result<Signed<RootSigned>> {
    vesper_meta::verify_root_self(anchor).context("trusted root")?;
    let cache = trust_dir.join(format!(
        "root-{}.json",
        &vesper_meta::metadata_hash(anchor)?[..16]
    ));
    let mut trusted = anchor.clone();
    match load_cached_root(&cache) {
        Ok(Some(cached)) if cached.signed.version > anchor.signed.version => trusted = cached,
        Ok(_) => {}
        Err(error) => eprintln!("ignoring unusable cached catalogue root: {error:#}"),
    }
    for _ in 0..MAX_ROOT_ROTATIONS {
        let version = trusted
            .signed
            .version
            .checked_add(1)
            .context("root version overflow")?;
        let name = vesper_meta::versioned_root_name(version);
        let Some(next) = fetch(&name).with_context(|| format!("fetch {name}"))? else {
            break;
        };
        vesper_meta::verify_root_successor(&trusted, &next)
            .with_context(|| format!("rejected {name}"))?;
        if vesper_meta::online_roles_changed(&trusted.signed, &next.signed) {
            forget_online_metadata(metadata_dir);
        }
        fs::create_dir_all(trust_dir)?;
        let temp = cache.with_extension("json.tmp");
        fs::write(&temp, canonical_bytes(&next)?)?;
        fs::rename(&temp, &cache)?;
        eprintln!("catalogue root updated to version {}", next.signed.version);
        trusted = next;
    }
    if trusted.signed.expires <= now {
        anyhow::bail!(
            "catalogue root version {} expired at {}; the index has not published a renewed root",
            trusted.signed.version,
            trusted.signed.expires
        );
    }
    let published = fetch("root.json")
        .context("fetch root.json")?
        .context("index has no root.json")?;
    if serde_json::to_value(&published)? != serde_json::to_value(&trusted)? {
        anyhow::bail!(
            "index root.json (version {}) does not match the verified root chain (version {}); \
             a lower version is a rollback and a higher one needs its versioned root file",
            published.signed.version,
            trusted.signed.version
        );
    }
    Ok(trusted)
}

fn load_cached_root(path: &std::path::Path) -> Result<Option<Signed<RootSigned>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let root: Signed<RootSigned> = serde_json::from_slice(&bytes)?;
    vesper_meta::verify_root_self(&root)?;
    Ok(Some(root))
}

/// A rotated snapshot/timestamp key invalidates cached online versions (TUF
/// fast-forward recovery): the new key starts a fresh version sequence.
fn forget_online_metadata(metadata_dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(metadata_dir) else {
        return;
    };
    for entry in entries.flatten() {
        for name in ["snapshot.json", "timestamp.json"] {
            let _ = fs::remove_file(entry.path().join(name));
        }
    }
}

fn fetch_optional_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<Option<T>> {
    let response = client()?.get(url).send()?;
    if matches!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE
    ) {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.json()?))
}

fn trusted_root() -> Result<Signed<RootSigned>> {
    match std::env::var_os(TRUSTED_ROOT_ENV) {
        Some(path) if !path.is_empty() => {
            let path = std::path::PathBuf::from(path);
            let bytes = fs::read(&path)
                .with_context(|| format!("read trusted root override {}", path.display()))?;
            Ok(serde_json::from_slice(&bytes)
                .with_context(|| format!("parse trusted root override {}", path.display()))?)
        }
        _ => Ok(serde_json::from_str(BUILTIN_ROOT)?),
    }
}

fn cached_version(app_id: &str) -> Option<u64> {
    fs::read_to_string(state_dir().join("metadata").join(app_id).join("version"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
}

fn cached_online_version(app_id: &str, name: &str) -> Result<Option<u64>> {
    let path = state_dir().join("metadata").join(app_id).join(name);
    if !path.exists() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(Some(
        value["signed"]["version"]
            .as_u64()
            .context("cached online version missing")?,
    ))
}

fn cache_metadata<T: Serialize>(app_id: &str, name: &str, value: &T) -> Result<()> {
    let dir = state_dir().join("metadata").join(app_id);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(name), canonical_bytes(value)?)?;
    Ok(())
}

fn fetch_targets(base: &str, app_id: &str) -> Result<(Signed<AppTargetsSigned>, AppRoute)> {
    let (root, routes) = metadata_root(base)?;
    let route = routes
        .signed
        .apps
        .get(app_id)
        .cloned()
        .context("application is not present in routes.json")?;
    let targets: Signed<AppTargetsSigned> =
        get_json(&join_url(base, &format!("apps/{app_id}/targets.json")))?;
    verify_app_targets(&targets, &route, cached_version(app_id))?;
    let timestamp: Signed<TimestampSigned> =
        get_json(&join_url(base, &format!("apps/{app_id}/timestamp.json")))?;
    let snapshot: Signed<SnapshotSigned> =
        get_json(&join_url(base, &format!("apps/{app_id}/snapshot.json")))?;
    check_version(
        timestamp.signed.version,
        timestamp.signed.expires,
        vesper_meta::now_unix(),
        cached_online_version(app_id, "timestamp.json")?,
    )?;
    check_version(
        snapshot.signed.version,
        snapshot.signed.expires,
        vesper_meta::now_unix(),
        cached_online_version(app_id, "snapshot.json")?,
    )?;
    verify_online_role(&snapshot, &root, "snapshot")?;
    verify_online_role(&timestamp, &root, "timestamp")?;
    vesper_meta::verify_app_chain(&targets, &snapshot, &timestamp, &root)?;
    cache_metadata(app_id, "targets.json", &targets)?;
    cache_metadata(app_id, "snapshot.json", &snapshot)?;
    cache_metadata(app_id, "timestamp.json", &timestamp)?;
    let dir = state_dir().join("metadata").join(app_id);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("version"), targets.signed.version.to_string())?;
    Ok((targets, route))
}

pub(crate) fn source_bases(
    base: &str,
    targets: &AppTargetsSigned,
    channel: &str,
    enabled: &[String],
) -> Result<Vec<SourceSpec>> {
    let release = targets
        .channels
        .get(channel)
        .context("requested channel is not published")?
        .manifest
        .release_tag
        .clone();
    let mut sources = targets
        .sources
        .iter()
        .filter(|source| {
            enabled.is_empty()
                || enabled
                    .iter()
                    .any(|id| id == &source.id || id == &source.kind)
        })
        .cloned()
        .collect::<Vec<_>>();
    sources.sort_by_key(|s| s.priority);
    if sources.is_empty() {
        anyhow::bail!("no enabled download source is available")
    }
    sources
        .into_iter()
        .map(|source| {
            let url = if source.url_template.starts_with("http://")
                || source.url_template.starts_with("https://")
            {
                source.url_template.clone()
            } else {
                join_url(base, &source.url_template)
            };
            let mut spec = SourceSpec::with_default_release(Source::Http(url), release.clone());
            spec.id = source.id;
            Ok(spec)
        })
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SourceRanking {
    recorded_at: u64,
    ids: Vec<String>,
}

fn apply_ranking(specs: &mut [SourceSpec], ranking: &SourceRanking, now: u64) {
    if now < ranking.recorded_at || now - ranking.recorded_at > 24 * 3600 {
        return;
    }
    specs.sort_by_key(|s| {
        ranking
            .ids
            .iter()
            .position(|id| id == &s.id)
            .unwrap_or(usize::MAX)
    });
}

fn apply_cached_ranking(specs: &mut [SourceSpec], path: &std::path::Path, now: u64) {
    if let Ok(bytes) = fs::read(path) {
        if let Ok(ranking) = serde_json::from_slice::<SourceRanking>(&bytes) {
            apply_ranking(specs, &ranking, now);
        }
    }
}

pub(crate) fn ranked_source_bases(
    base: &str,
    app_id: &str,
    targets: &AppTargetsSigned,
    channel: &str,
    enabled: &[String],
) -> Result<Vec<SourceSpec>> {
    let mut specs = source_bases(base, targets, channel, enabled)?;
    let path = state_dir()
        .join("metadata")
        .join(app_id)
        .join("source-ranking.json");
    apply_cached_ranking(&mut specs, &path, vesper_meta::now_unix());
    Ok(specs)
}

pub(crate) fn cache_source_ranking(app_id: &str, ids: Vec<String>) {
    if let Err(error) = cache_metadata(
        app_id,
        "source-ranking.json",
        &SourceRanking {
            recorded_at: vesper_meta::now_unix(),
            ids,
        },
    ) {
        eprintln!("{app_id}: source ranking cache failed: {error:#}");
    }
}

#[cfg(test)]
fn manifest_url(
    base: &str,
    targets: &AppTargetsSigned,
    channel: &str,
    enabled: &[String],
) -> Result<String> {
    let source = source_bases(base, targets, channel, enabled)?
        .into_iter()
        .next()
        .context("no manifest source")?;
    let Source::Http(template) = source.source else {
        anyhow::bail!("manifest requires HTTP");
    };
    let manifest = &targets.channels[channel].manifest;
    Ok(template
        .replace("{release}", &manifest.release_tag)
        .replace("{pack_id}", &manifest.asset_name))
}

pub(crate) fn fetch_manifest_bytes(
    base: &str,
    app_id: &str,
    targets: &AppTargetsSigned,
    channel: &str,
    enabled: &[String],
) -> Result<Vec<u8>> {
    let channel_meta = targets
        .channels
        .get(channel)
        .context("requested channel is not published")?;
    let candidates = ranked_source_bases(base, app_id, targets, channel, enabled)?
        .into_iter()
        .filter_map(|spec| match spec.source {
            Source::Http(template) => Some((
                spec.id,
                template
                    .replace("{release}", &channel_meta.manifest.release_tag)
                    .replace("{pack_id}", &channel_meta.manifest.asset_name),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut last_error = None;
    for (id, url) in candidates {
        match client()?
            .get(&url)
            .send()
            .and_then(|response| response.error_for_status())
        {
            Ok(response) => match response.bytes() {
                Ok(bytes)
                    if bytes.len() as u64 == channel_meta.manifest.size
                        && hex::encode(vesper_format::hash_bytes(&bytes))
                            == channel_meta.manifest.blake3 =>
                {
                    return Ok(bytes.to_vec())
                }
                Ok(bytes) => {
                    last_error = Some(anyhow::anyhow!(
                        "manifest length/hash mismatch from {url} ({} bytes)",
                        bytes.len()
                    ));
                }
                Err(error) => last_error = Some(error.into()),
            },
            Err(error) => last_error = Some(error.into()),
        }
        eprintln!(
            "{app_id}: manifest source={id} rejected: {}",
            last_error.as_ref().unwrap()
        );
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("manifest was not found")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vesper_meta::{AppChannel, AppChannelManifest, AppSource, KeyPair};

    static TRUST_ROOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An in-memory index serving `root.json` and versioned roots.
    struct RootIndex {
        files: BTreeMap<String, Signed<RootSigned>>,
        requested: Vec<String>,
    }

    impl RootIndex {
        fn new(current: &Signed<RootSigned>, chain: &[&Signed<RootSigned>]) -> Self {
            let mut files = BTreeMap::from([("root.json".to_string(), current.clone())]);
            for root in chain {
                files.insert(
                    vesper_meta::versioned_root_name(root.signed.version),
                    (*root).clone(),
                );
            }
            Self {
                files,
                requested: Vec::new(),
            }
        }

        fn update(
            &mut self,
            anchor: &Signed<RootSigned>,
            state: &std::path::Path,
            now: u64,
        ) -> Result<Signed<RootSigned>> {
            update_root(
                anchor,
                &state.join("trust"),
                &state.join("metadata"),
                now,
                |name| {
                    self.requested.push(name.to_string());
                    Ok(self.files.get(name).cloned())
                },
            )
        }
    }

    const NOW: u64 = 1_800_000_000;

    fn root_payload(
        version: u64,
        expires: u64,
        root_keys: &[&KeyPair],
        threshold: u32,
        online: &KeyPair,
    ) -> RootSigned {
        let mut keys: BTreeMap<String, String> = root_keys
            .iter()
            .map(|key| (key.keyid.clone(), key.public_hex()))
            .collect();
        keys.insert(online.keyid.clone(), online.public_hex());
        let online_role = vesper_meta::RoleSpec {
            keyids: vec![online.keyid.clone()],
            threshold: 1,
        };
        RootSigned {
            typ: "root".into(),
            spec_version: "1.0.0".into(),
            version,
            expires,
            keys,
            roles: BTreeMap::from([
                (
                    "root".into(),
                    vesper_meta::RoleSpec {
                        keyids: root_keys.iter().map(|key| key.keyid.clone()).collect(),
                        threshold,
                    },
                ),
                ("snapshot".into(), online_role.clone()),
                ("timestamp".into(), online_role),
            ]),
        }
    }

    fn signed_root(payload: RootSigned, signers: &[&KeyPair]) -> Signed<RootSigned> {
        vesper_meta::sign(payload, signers).unwrap()
    }

    fn keys<const N: usize>() -> [KeyPair; N] {
        std::array::from_fn(|_| KeyPair::generate())
    }

    #[test]
    fn embedded_official_root_is_self_signed() {
        let official: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT).unwrap();
        vesper_meta::verify_root_self(&official).unwrap();
    }

    #[test]
    fn unchanged_root_is_accepted_and_different_root_is_rejected() {
        let [a, b, c, online] = keys::<4>();
        let v1 = signed_root(
            root_payload(1, NOW + 100, &[&a, &b, &c], 2, &online),
            &[&a, &b],
        );
        let state = tempfile::tempdir().unwrap();
        let mut index = RootIndex::new(&v1, &[]);
        assert_eq!(
            index.update(&v1, state.path(), NOW).unwrap().signed.version,
            1
        );
        assert_eq!(index.requested, ["2.root.json", "root.json"]);

        // A different self-signed root that does not chain from the anchor.
        let [x, y] = keys::<2>();
        let other = signed_root(root_payload(1, NOW + 100, &[&x, &y], 2, &online), &[&x, &y]);
        let error = RootIndex::new(&other, &[])
            .update(&v1, state.path(), NOW)
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not match the verified root chain"));
    }

    #[test]
    fn renewed_root_is_followed_and_cached() {
        let [a, b, c, online] = keys::<4>();
        // The anchor is already expired; renewal by the same keys recovers it.
        let v1 = signed_root(
            root_payload(1, NOW - 1, &[&a, &b, &c], 2, &online),
            &[&a, &b],
        );
        let v2 = signed_root(
            root_payload(2, NOW + 365 * 86400, &[&a, &b, &c], 2, &online),
            &[&b, &c],
        );
        let state = tempfile::tempdir().unwrap();
        let error = RootIndex::new(&v1, &[&v1])
            .update(&v1, state.path(), NOW)
            .unwrap_err();
        assert!(format!("{error:#}").contains("expired"));

        let mut index = RootIndex::new(&v2, &[&v1, &v2]);
        let trusted = index.update(&v1, state.path(), NOW).unwrap();
        assert_eq!(trusted.signed.version, 2);
        assert_eq!(index.requested, ["2.root.json", "3.root.json", "root.json"]);

        // The next start begins from the cached version 2 root.
        let mut index = RootIndex::new(&v2, &[&v1, &v2]);
        assert_eq!(
            index.update(&v1, state.path(), NOW).unwrap().signed.version,
            2
        );
        assert_eq!(index.requested, ["3.root.json", "root.json"]);
    }

    #[test]
    fn rotated_root_keys_are_trusted_through_the_chain() {
        let [a, b, c, d, e, f, online, online2] = keys::<8>();
        let v1 = signed_root(
            root_payload(1, NOW + 100, &[&a, &b, &c], 2, &online),
            &[&a, &b],
        );
        // Version 2 replaces every root key and the online key; it is signed by
        // a threshold of the old keys and a threshold of the new keys.
        let v2 = signed_root(
            root_payload(2, NOW + 100, &[&d, &e, &f], 2, &online2),
            &[&a, &c, &d, &e],
        );
        // Version 3 is signed only by the new keys.
        let v3 = signed_root(
            root_payload(3, NOW + 200, &[&d, &e, &f], 2, &online2),
            &[&e, &f],
        );
        let state = tempfile::tempdir().unwrap();
        let cached_online = state.path().join("metadata/some.app");
        fs::create_dir_all(&cached_online).unwrap();
        fs::write(cached_online.join("timestamp.json"), "{}").unwrap();
        fs::write(cached_online.join("targets.json"), "{}").unwrap();

        let trusted = RootIndex::new(&v3, &[&v1, &v2, &v3])
            .update(&v1, state.path(), NOW)
            .unwrap();
        assert_eq!(trusted.signed.version, 3);
        assert!(trusted.signed.keys.contains_key(&online2.keyid));
        // The rotated online key starts a fresh snapshot/timestamp sequence.
        assert!(!cached_online.join("timestamp.json").exists());
        assert!(cached_online.join("targets.json").exists());
    }

    #[test]
    fn forged_next_root_is_rejected_and_not_cached() {
        let [a, b, c, online, attacker1, attacker2] = keys::<6>();
        let v1 = signed_root(
            root_payload(1, NOW + 100, &[&a, &b, &c], 2, &online),
            &[&a, &b],
        );
        let forgeries = [
            // Self-signed by attacker keys only.
            signed_root(
                root_payload(2, NOW + 100, &[&attacker1, &attacker2], 2, &online),
                &[&attacker1, &attacker2],
            ),
            // One stolen old key is below the previous threshold.
            signed_root(
                root_payload(2, NOW + 100, &[&a, &attacker1], 1, &online),
                &[&a, &attacker1],
            ),
            // A threshold of old keys, but not of the new root's own keys.
            signed_root(
                root_payload(2, NOW + 100, &[&attacker1, &attacker2], 2, &online),
                &[&a, &b],
            ),
        ];
        for forged in forgeries {
            let state = tempfile::tempdir().unwrap();
            let error = RootIndex::new(&forged, &[&v1, &forged])
                .update(&v1, state.path(), NOW)
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("rejected 2.root.json"),
                "{error:#}"
            );
            assert!(!state.path().join("trust").exists());
        }
        // A tampered payload invalidates an otherwise valid signature.
        let mut tampered = signed_root(
            root_payload(2, NOW + 100, &[&a, &b, &c], 2, &online),
            &[&a, &b],
        );
        tampered.signed.expires += 1;
        let state = tempfile::tempdir().unwrap();
        assert!(RootIndex::new(&tampered, &[&tampered])
            .update(&v1, state.path(), NOW)
            .is_err());
    }

    #[test]
    fn root_rollback_and_version_gaps_are_rejected() {
        let [a, b, c, online] = keys::<4>();
        let payload = |version| root_payload(version, NOW + 100, &[&a, &b, &c], 2, &online);
        let v1 = signed_root(payload(1), &[&a, &b]);
        let v2 = signed_root(payload(2), &[&a, &b]);
        let v3 = signed_root(payload(3), &[&a, &b]);
        let state = tempfile::tempdir().unwrap();
        RootIndex::new(&v3, &[&v1, &v2, &v3])
            .update(&v1, state.path(), NOW)
            .unwrap();

        // After version 3 is trusted, an index rolled back to version 2 fails.
        let error = RootIndex::new(&v2, &[&v1, &v2])
            .update(&v1, state.path(), NOW)
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not match the verified root chain"));

        // Versioned files must hold exactly the next version.
        let fresh = tempfile::tempdir().unwrap();
        let mut gap = RootIndex::new(&v3, &[&v1]);
        gap.files.insert("2.root.json".into(), v3.clone());
        let error = gap.update(&v1, fresh.path(), NOW).unwrap_err();
        assert!(
            format!("{error:#}").contains("rollback or gap"),
            "{error:#}"
        );
        let mut rollback = RootIndex::new(&v1, &[&v1]);
        rollback.files.insert("2.root.json".into(), v1.clone());
        let error = rollback.update(&v1, fresh.path(), NOW).unwrap_err();
        assert!(
            format!("{error:#}").contains("rollback or gap"),
            "{error:#}"
        );

        // A newer root.json without its versioned file cannot be trusted.
        let error = RootIndex::new(&v3, &[&v1, &v2])
            .update(&v1, fresh.path(), NOW)
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not match the verified root chain"));
    }

    #[test]
    fn newer_trust_anchor_supersedes_older_cache() {
        let [a, b, c, online] = keys::<4>();
        let payload = |version| root_payload(version, NOW + 100, &[&a, &b, &c], 2, &online);
        let v1 = signed_root(payload(1), &[&a, &b]);
        let v2 = signed_root(payload(2), &[&a, &b]);
        let state = tempfile::tempdir().unwrap();
        // A client release that embeds version 2 does not need 2.root.json.
        let mut index = RootIndex::new(&v2, &[&v1]);
        assert_eq!(
            index.update(&v2, state.path(), NOW).unwrap().signed.version,
            2
        );
        assert_eq!(index.requested, ["3.root.json", "root.json"]);
    }

    #[test]
    fn development_override_replaces_the_embedded_anchor() {
        let _lock = TRUST_ROOT_ENV_LOCK.lock().unwrap();
        let [a, online] = keys::<2>();
        let development = signed_root(root_payload(1, NOW + 100, &[&a], 1, &online), &[&a]);
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), serde_json::to_vec(&development).unwrap()).unwrap();
        let previous = std::env::var_os(TRUSTED_ROOT_ENV);
        std::env::set_var(TRUSTED_ROOT_ENV, temp.path());
        let anchor = trusted_root().unwrap();
        if let Some(previous) = previous {
            std::env::set_var(TRUSTED_ROOT_ENV, previous);
        } else {
            std::env::remove_var(TRUSTED_ROOT_ENV);
        }
        assert!(anchor.signed.keys.contains_key(&a.keyid));
        let state = tempfile::tempdir().unwrap();
        RootIndex::new(&development, &[])
            .update(&anchor, state.path(), NOW)
            .unwrap();
    }

    #[test]
    fn manifest_404_or_tampering_falls_back_to_next_enabled_source() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };
        fn server(status: u16, body: &'static [u8]) -> (String, thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "http://{}/{{release}}/{{pack_id}}",
                listener.local_addr().unwrap()
            );
            let worker = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buf = [0u8; 1024];
                while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut buf).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&buf[..n]);
                }
                write!(
                    socket,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                socket.write_all(body).unwrap();
            });
            (url, worker)
        }
        for status in [404, 200] {
            let (bad, first) = server(status, b"bad");
            let (good, second) = server(200, b"yes");
            let mut value = targets(&bad);
            value.channels.get_mut("stable").unwrap().manifest.size = 3;
            value.channels.get_mut("stable").unwrap().manifest.blake3 =
                hex::encode(vesper_format::hash_bytes(b"yes"));
            value.sources.push(AppSource {
                id: "r2".into(),
                kind: "r2".into(),
                url_template: good,
                priority: 1,
            });
            assert_eq!(
                fetch_manifest_bytes(
                    "http://127.0.0.1:1",
                    "test.manifest-fallback",
                    &value,
                    "stable",
                    &[]
                )
                .unwrap(),
                b"yes"
            );
            first.join().unwrap();
            second.join().unwrap();
        }
    }

    #[test]
    fn second_download_uses_persisted_ranking_until_expiry_and_respects_enabled_sources() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source-ranking.json");
        let mut value = targets("https://example.com/{release}/{pack_id}");
        value.sources.push(AppSource {
            id: "r2".into(),
            kind: "r2".into(),
            url_template: "https://cdn.example.com/{release}/{pack_id}".into(),
            priority: 1,
        });
        let ranking = SourceRanking {
            recorded_at: 100,
            ids: vec!["r2".into(), "github".into()],
        };
        fs::write(&path, serde_json::to_vec(&ranking).unwrap()).unwrap();
        let mut next = source_bases("https://example.com", &value, "stable", &[]).unwrap();
        apply_cached_ranking(&mut next, &path, 200);
        assert_eq!(next[0].id, "r2");
        let mut expired = source_bases("https://example.com", &value, "stable", &[]).unwrap();
        apply_cached_ranking(&mut expired, &path, 100 + 86401);
        assert_eq!(expired[0].id, "github");
        let mut disabled =
            source_bases("https://example.com", &value, "stable", &["github".into()]).unwrap();
        apply_cached_ranking(&mut disabled, &path, 200);
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].id, "github");
    }

    #[test]
    fn online_roles_verify_signatures_and_require_roles_outside_bootstrap() {
        let key = vesper_meta::KeyPair::generate();
        let mut root: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT).unwrap();
        root.signed.keys.insert(key.keyid.clone(), key.public_hex());
        let spec = vesper_meta::RoleSpec {
            keyids: vec![key.keyid.clone()],
            threshold: 1,
        };
        root.signed.roles.insert("timestamp".into(), spec.clone());
        root.signed.roles.insert("snapshot".into(), spec);
        let mut timestamp = vesper_meta::sign(
            TimestampSigned {
                typ: "timestamp".into(),
                spec_version: "1.0.0".into(),
                version: 1,
                expires: vesper_meta::expires_in(1),
                meta: BTreeMap::new(),
            },
            &[&key],
        )
        .unwrap();
        verify_online_role_with_bootstrap(&timestamp, &root, "timestamp", false).unwrap();
        timestamp.signed.version += 1;
        assert!(verify_online_role_with_bootstrap(&timestamp, &root, "timestamp", true).is_err());
        root.signed.roles.remove("timestamp");
        assert!(verify_online_role_with_bootstrap(&timestamp, &root, "timestamp", false).is_err());
        verify_online_role_with_bootstrap(&timestamp, &root, "timestamp", true).unwrap();
        root.signed.roles.remove("snapshot");
        assert!(verify_online_role_with_bootstrap(&timestamp, &root, "snapshot", false).is_err());
    }

    fn targets(template: &str) -> AppTargetsSigned {
        let mut channels = BTreeMap::new();
        channels.insert(
            "stable".into(),
            AppChannel {
                version: 1,
                platform: "windows-x64".into(),
                manifest: AppChannelManifest {
                    release_tag: "ghtest-1".into(),
                    asset_name: "manifest.cbor.zst".into(),
                    size: 1,
                    blake3: "00".repeat(32),
                },
                encrypted: false,
                key_id: 0,
            },
        );
        AppTargetsSigned {
            metadata: ["store.json", "description.md"]
                .into_iter()
                .map(|name| (name.into(), vesper_meta::store_file_description(b"{}")))
                .collect(),
            typ: "targets".into(),
            spec_version: "1.0.0".into(),
            version: 1,
            expires: vesper_meta::expires_in(90),
            channels,
            sources: vec![AppSource {
                id: "github".into(),
                kind: "github".into(),
                url_template: template.into(),
                priority: 0,
            }],
        }
    }

    #[test]
    fn absolute_source_template_reaches_flat_release_asset() {
        let value = targets("https://github.com/o/r/releases/download/{release}/{pack_id}");
        let sources = source_bases("http://127.0.0.1:3210", &value, "stable", &[]).unwrap();
        assert_eq!(
            sources[0].source.id(),
            "http:https://github.com/o/r/releases/download/{release}/{pack_id}"
        );
        assert_eq!(
            manifest_url("http://127.0.0.1:3210", &value, "stable", &[]).unwrap(),
            "https://github.com/o/r/releases/download/ghtest-1/manifest.cbor.zst"
        );
    }

    #[test]
    fn relative_source_template_stays_relative_to_index_api() {
        let value = targets("releases/{release}/{pack_id}");
        let sources = source_bases("http://127.0.0.1:3210/index", &value, "stable", &[]).unwrap();
        assert_eq!(
            sources[0].source.id(),
            "http:http://127.0.0.1:3210/index/releases/{release}/{pack_id}"
        );
    }
}
