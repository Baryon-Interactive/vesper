//! Bounded developer metadata transport and validation shared by clients and aggregation.
use crate::{AppRoute, AppTargetsSigned, Signed};
use anyhow::{Context, Result};
#[cfg(not(target_arch = "wasm32"))]
use reqwest::{blocking::Client, redirect::Policy};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Read, path::Path, time::Duration};
use url::Url;

pub const FILES: [&str; 3] = ["targets.json", "store.json", "description.md"];
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
    anyhow::ensure!(FILES.contains(&name), "unsupported metadata file");
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
    pub files: [Vec<u8>; 3],
}

impl Bundle {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn fetch(
        base: &str,
        route: &AppRoute,
        previous: Option<&Signed<AppTargetsSigned>>,
    ) -> Result<Self> {
        let files = [
            fetch_file(base, FILES[0])?,
            fetch_file(base, FILES[1])?,
            fetch_file(base, FILES[2])?,
        ];
        let targets = serde_json::from_slice(&files[0]).context("parse developer targets")?;
        let bundle = Self { targets, files };
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
        for (name, bytes) in FILES[1..].iter().zip(&self.files[1..]) {
            crate::verify_store_bytes(&self.targets.signed, name, bytes)?;
        }
        let store: serde_json::Value =
            serde_json::from_slice(&self.files[1]).context("parse store.json")?;
        anyhow::ensure!(store.is_object(), "store.json must be an object");
        if let Some(tags) = store.get("tags") {
            anyhow::ensure!(
                tags.as_array().is_some_and(|tags| tags.iter().all(|tag| {
                    tag.as_str().is_some_and(|tag| {
                        !tag.is_empty()
                            && tag.len() <= 32
                            && tag
                                .bytes()
                                .next()
                                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                            && tag
                                .bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                    })
                })),
                "invalid catalogue tags"
            );
        }
        std::str::from_utf8(&self.files[2]).context("description.md must be UTF-8")?;
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
    if previous
        .as_ref()
        .is_some_and(|p| p.signed.version == bundle.targets.signed.version)
    {
        // Repair damaged/missing store files even when the signed payload is equal.
        if FILES[1..]
            .iter()
            .zip(&bundle.files[1..])
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
        for (name, bytes) in FILES.iter().zip(&bundle.files) {
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
