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

/// Verify and cache the signed metadata chain for an application.
pub fn verify_app(base: &str, app_id: &str) -> Result<(Signed<AppTargetsSigned>, AppRoute)> {
    fetch_targets(base, app_id)
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
    let builtin: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT)?;
    verify_online_role_with_bootstrap(document, root, role, builtin.signed.keys.is_empty())
}

fn verify_online_role_with_bootstrap<T: Serialize>(
    document: &Signed<T>,
    root: &Signed<RootSigned>,
    role: &str,
    bootstrap: bool,
) -> Result<()> {
    if let Some(spec) = root.signed.roles.get(role) {
        verify_role(document, &root.signed.keys, spec)
    } else if bootstrap {
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
    // A production build can replace config/root.json with the offline root. The
    // empty bootstrap root is deliberately accepted for local demo indexes.
    let builtin: Signed<RootSigned> = serde_json::from_str(BUILTIN_ROOT)?;
    if !builtin.signed.keys.is_empty()
        && (builtin.signed.keys != root.signed.keys
            || serde_json::to_value(&builtin.signed.roles)?
                != serde_json::to_value(&root.signed.roles)?)
    {
        anyhow::bail!("index root does not match the launcher's trusted root")
    }
    if let Some(role) = root.signed.roles.get("root") {
        verify_role(&root, &root.signed.keys, role)?;
    }
    verify_routes(&routes, &root)?;
    Ok((root, routes))
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
            Ok(SourceSpec::with_default_release(
                Source::Http(url),
                release.clone(),
            ))
        })
        .collect()
}

fn manifest_url(
    base: &str,
    targets: &AppTargetsSigned,
    channel: &str,
    enabled: &[String],
) -> Result<String> {
    let channel = targets
        .channels
        .get(channel)
        .context("requested channel is not published")?;
    let source = targets
        .sources
        .iter()
        .filter(|source| {
            enabled.is_empty()
                || enabled
                    .iter()
                    .any(|id| id == &source.id || id == &source.kind)
        })
        .min_by_key(|source| source.priority)
        .context("no manifest source is published")?;
    let url = source
        .url_template
        .replace("{release}", &channel.manifest.release_tag)
        .replace("{pack_id}", &channel.manifest.asset_name);
    Ok(
        if url.starts_with("http://") || url.starts_with("https://") {
            url
        } else {
            join_url(base, &url)
        },
    )
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
    let candidates = [
        manifest_url(base, targets, channel, enabled)?,
        join_url(
            base,
            &format!("apps/{app_id}/{}", channel_meta.manifest.asset_name),
        ),
    ];
    let mut last_error = None;
    for url in candidates {
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
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("manifest was not found")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vesper_meta::{AppChannel, AppChannelManifest, AppSource};

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
