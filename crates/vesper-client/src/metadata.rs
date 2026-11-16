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
    let files = [
        canonical_bytes(&targets)?,
        vesper_meta::hosting::fetch_file(&join_url(base, &format!("apps/{app_id}")), "store.json")?,
        vesper_meta::hosting::fetch_file(
            &join_url(base, &format!("apps/{app_id}")),
            "description.md",
        )?,
    ];
    vesper_meta::hosting::Bundle {
        targets: targets.clone(),
        files,
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
    let root: Signed<RootSigned> = get_json(&join_url(base, "root.json"))?;
    let routes: Signed<RoutesSigned> = get_json(&join_url(base, "routes.json"))?;
    let trusted = trusted_root()?;
    if !root_matches_trusted(&root, &trusted)? {
        anyhow::bail!("index root does not match the launcher's trusted root")
    }
    if let Some(role) = root.signed.roles.get("root") {
        verify_role(&root, &root.signed.keys, role)?;
    }
    verify_routes(&routes, &root)?;
    Ok((root, routes))
}

fn root_matches_trusted(root: &Signed<RootSigned>, trusted: &Signed<RootSigned>) -> Result<bool> {
    Ok(serde_json::to_value(root)? == serde_json::to_value(trusted)?)
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
    use vesper_meta::{AppChannel, AppChannelManifest, AppSource};

    static TRUST_ROOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn official_root_is_accepted() {
        let official: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT).unwrap();
        assert!(root_matches_trusted(&official, &official).unwrap());
    }

    #[test]
    fn different_root_is_rejected_without_override() {
        let official: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT).unwrap();
        let mut different = official.clone();
        different
            .signed
            .keys
            .insert("different".into(), "key".into());
        assert!(!root_matches_trusted(&different, &official).unwrap());
    }

    #[test]
    fn different_root_is_accepted_with_explicit_override() {
        let _lock = TRUST_ROOT_ENV_LOCK.lock().unwrap();
        let official: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT).unwrap();
        let mut development = official.clone();
        development
            .signed
            .keys
            .insert("different".into(), "key".into());
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), serde_json::to_vec(&development).unwrap()).unwrap();
        let previous = std::env::var_os(TRUSTED_ROOT_ENV);
        std::env::set_var(TRUSTED_ROOT_ENV, temp.path());
        assert!(root_matches_trusted(&development, &trusted_root().unwrap()).unwrap());
        if let Some(previous) = previous {
            std::env::set_var(TRUSTED_ROOT_ENV, previous);
        } else {
            std::env::remove_var(TRUSTED_ROOT_ENV);
        }
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
