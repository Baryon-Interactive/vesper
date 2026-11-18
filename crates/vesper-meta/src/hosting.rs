//! Bounded developer metadata transport and validation shared by clients and aggregation.
use crate::{AppRoute, AppTargetsSigned, Signed};
use anyhow::{Context, Result};
#[cfg(not(target_arch = "wasm32"))]
use reqwest::{blocking::Client, redirect::Policy};
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Read, path::Path, time::Duration};
use url::Url;

/// Files every developer metadata directory serves; more store files
/// (changelogs, per-language descriptions) are listed in the signed targets.
pub const FILES: [&str; 3] = ["targets.json", "store.json", "description.md"];

/// Whether `name` may be fetched from developer metadata hosting.
pub fn is_metadata_file(name: &str) -> bool {
    name == "targets.json" || crate::store::is_store_file_name(name)
}
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

pub fn validate_app_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id != "."
            && id != ".."
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid application id"
    );
    Ok(())
}

fn allowed_url(url: &Url, local: bool) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && (url.scheme() == "https"
            || (local && url.scheme() == "http" && url.host_str() == Some("127.0.0.1")))
}

pub fn metadata_base(value: &str) -> Result<Url> {
    let url = Url::parse(&format!("{}/", value.trim_end_matches('/')))?;
    anyhow::ensure!(
        allowed_url(&url, true),
        "metadata URL requires HTTPS (or HTTP 127.0.0.1 for local tests)"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "metadata base cannot contain query or fragment"
    );
    Ok(url)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_file(base: &str, name: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(is_metadata_file(name), "unsupported metadata file");
    let base = metadata_base(base)?;
    // Only a loopback-origin mock may redirect to loopback HTTP. Production
    // requests cannot downgrade to HTTP, including a loopback destination.
    let local = base.scheme() == "http";
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 || !allowed_url(attempt.url(), local) {
                attempt.error("metadata redirect requires HTTPS and at most five hops")
            } else {
                attempt.follow()
            }
        }));
    if local {
        builder = builder.no_proxy();
    }
    let client = builder.build()?;
    let response = client.get(base.join(name)?).send()?.error_for_status()?;
    anyhow::ensure!(
        response.content_length().is_none_or(|n| n <= MAX_FILE_SIZE),
        "metadata file exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    response.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_FILE_SIZE,
        "metadata file exceeds 1 MiB"
    );
    Ok(bytes)
}

pub struct Bundle {
    pub targets: Signed<AppTargetsSigned>,
    /// Exact bytes of `targets.json` as served.
    pub targets_bytes: Vec<u8>,
    /// Every store file the targets sign, by name.
    pub store: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn fetch(
        base: &str,
        route: &AppRoute,
        previous: Option<&Signed<AppTargetsSigned>>,
    ) -> Result<Self> {
        let targets_bytes = fetch_file(base, FILES[0])?;
        let targets: Signed<AppTargetsSigned> =
            serde_json::from_slice(&targets_bytes).context("parse developer targets")?;
        // Check the signature before following the file list it contains.
        crate::verify_app_targets(&targets, route, previous.map(|p| p.signed.version))?;
        let store = targets
            .signed
            .metadata
            .keys()
            .map(|name| Ok((name.clone(), fetch_file(base, name)?)))
            .collect::<Result<_>>()?;
        let bundle = Self {
            targets,
            targets_bytes,
            store,
        };
        bundle.verify(route, previous)?;
        Ok(bundle)
    }

    pub fn verify(
        &self,
        route: &AppRoute,
        previous: Option<&Signed<AppTargetsSigned>>,
    ) -> Result<()> {
        crate::verify_app_targets(&self.targets, route, previous.map(|p| p.signed.version))?;
        if let Some(previous) = previous {
            if self.targets.signed.version == previous.signed.version {
                anyhow::ensure!(
                    crate::canonical_bytes(&self.targets.signed)?
                        == crate::canonical_bytes(&previous.signed)?,
                    "same targets version has different content"
                );
            }
        }
        anyhow::ensure!(
            self.store.keys().eq(self.targets.signed.metadata.keys()),
            "store files differ from the files the targets sign"
        );
        for (name, bytes) in &self.store {
            crate::verify_store_bytes(&self.targets.signed, name, bytes)?;
            if name.ends_with(".md") {
                anyhow::ensure!(
                    bytes.len() <= crate::store::MAX_MARKDOWN_BYTES,
                    "{name} exceeds {} bytes",
                    crate::store::MAX_MARKDOWN_BYTES
                );
                std::str::from_utf8(bytes).with_context(|| format!("{name} must be UTF-8"))?;
            }
        }
        let store: serde_json::Value =
            serde_json::from_slice(&self.store["store.json"]).context("parse store.json")?;
        anyhow::ensure!(store.is_object(), "store.json must be an object");
        if let Some(tags) = store.get("tags") {
            anyhow::ensure!(
                tags.as_array().is_some_and(|tags| tags
                    .iter()
                    .all(|tag| tag.as_str().is_some_and(crate::store::is_tag))),
                "invalid catalogue tags"
            );
        }
        Ok(())
    }

    /// Every listing rule from [`crate::store::validate`]; the catalogue gate.
    pub fn validate_listing(&self) -> Result<()> {
        let store: serde_json::Value =
            serde_json::from_slice(&self.store["store.json"]).context("parse store.json")?;
        let problems = crate::store::validate(&store);
        anyhow::ensure!(
            problems.is_empty(),
            "invalid store.json: {}",
            problems.join("; ")
        );
        Ok(())
    }
}

/// Validate every file before replacing an application's directory. Preserve its
/// central online roles until the signing pass replaces them.
#[cfg(not(target_arch = "wasm32"))]
pub fn aggregate_app(index: &Path, id: &str, route: &AppRoute) -> Result<&'static str> {
    validate_app_id(id)?;
    let base = route
        .metadata_url
        .as_deref()
        .context("metadata URL missing")?;
    let dir = index.join("apps").join(id);
    let previous: Option<Signed<AppTargetsSigned>> = if dir.join(FILES[0]).exists() {
        Some(crate::read_json(&dir.join(FILES[0]))?)
    } else {
        None
    };
    let bundle = Bundle::fetch(base, route, previous.as_ref())?;
    bundle.validate_listing()?;
    if previous
        .as_ref()
        .is_some_and(|p| p.signed.version == bundle.targets.signed.version)
    {
        // Repair damaged/missing store files even when the signed payload is equal.
        if bundle
            .store
            .iter()
            .all(|(name, bytes)| fs::read(dir.join(name)).ok().as_ref() == Some(bytes))
        {
            return Ok("unchanged");
        }
    }
    let staging = index.join(format!(".aggregate-stage-{id}"));
    let backup = index.join(format!(".aggregate-backup-{id}"));
    anyhow::ensure!(
        !staging.exists() && !backup.exists(),
        "unfinished aggregation directories require recovery"
    );
    fs::create_dir_all(&staging)?;
    let prepare = (|| -> Result<()> {
        for name in ["snapshot.json", "timestamp.json"] {
            if dir.join(name).exists() {
                fs::copy(dir.join(name), staging.join(name))?;
            }
        }
        fs::write(staging.join(FILES[0]), &bundle.targets_bytes)?;
        for (name, bytes) in &bundle.store {
            fs::write(staging.join(name), bytes)?;
        }
        fs::create_dir_all(index.join("apps"))?;
        Ok(())
    })();
    if let Err(error) = prepare {
        fs::remove_dir_all(&staging)?;
        return Err(error);
    }
    let existed = dir.exists();
    if existed {
        fs::rename(&dir, &backup)?;
    }
    if let Err(error) = fs::rename(&staging, &dir) {
        if existed {
            fs::rename(&backup, &dir)?;
        }
        fs::remove_dir_all(&staging)?;
        return Err(error.into());
    }
    if existed {
        fs::remove_dir_all(&backup)?;
    }
    Ok("updated")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_redirect_policy_cannot_downgrade_to_loopback_http() {
        assert!(!allowed_url(
            &Url::parse("http://127.0.0.1:80/targets.json").unwrap(),
            false
        ));
        assert!(!allowed_url(
            &Url::parse("http://example.test/targets.json").unwrap(),
            true
        ));
        assert!(allowed_url(
            &Url::parse("https://cdn.example.test/targets.json").unwrap(),
            false
        ));
    }
}
