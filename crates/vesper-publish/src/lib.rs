//! GitHub Releases publisher for developer-owned pack repositories.

pub mod r2;
mod transfer;

use anyhow::{Context, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE, USER_AGENT,
};
use serde::Deserialize;
use std::fs::{self, File};
use std::path::Path;
use std::thread;
use std::time::Duration;
use vesper_format::Manifest;

#[derive(Debug, Clone)]
pub struct GitHubPublisher {
    client: Client,
    api_base: String,
    upload_base: String,
    token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub id: u64,
    pub tag_name: String,
    pub upload_url: String,
    pub assets_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

#[derive(Debug, Clone)]
pub struct PublishResult {
    pub release: Release,
    pub uploaded: Vec<String>,
    pub skipped_existing: Vec<String>,
}

pub fn token_from_env_or_gh() -> Result<String> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            return Ok(token);
        }
    }
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("read GitHub token from GITHUB_TOKEN or gh auth token")?;
    if !output.status.success() {
        anyhow::bail!("gh auth token failed");
    }
    let token = String::from_utf8(output.stdout).context("gh returned a non-UTF-8 token")?;
    if token.trim().is_empty() {
        anyhow::bail!("GitHub token is empty");
    }
    Ok(token.trim().to_string())
}

impl GitHubPublisher {
    /// Public object recovery never forwards publishing credentials.
    pub fn recover_asset(
        &self,
        owner: &str,
        repo: &str,
        tag: &str,
        name: &str,
        identity: &r2::ObjectIdentity,
        destination: Option<&Path>,
    ) -> Result<()> {
        let size = identity.size;
        let release = self
            .get_release(owner, repo, tag)?
            .context("asset release is missing")?;
        let asset = self
            .list_assets(&release)?
            .into_iter()
            .find(|a| a.name == name)
            .context("asset is missing from release")?;
        anyhow::ensure!(asset.size == size, "asset length/hash mismatch");
        if let Some(path) = destination {
            let mut last = None;
            for attempt in 0..4 {
                match transfer::download(&asset.browser_download_url, path, size) {
                    Ok(()) => {
                        last = None;
                        break;
                    }
                    Err(error) => {
                        last = Some(error);
                        if attempt < 3 {
                            thread::sleep(Duration::from_millis(250 << attempt));
                        }
                    }
                }
            }
            if let Some(error) = last {
                return Err(error.context("recover GitHub asset after retries"));
            }
            anyhow::ensure!(
                hex::encode(vesper_format::hash_file(path)?) == identity.blake3,
                "asset length/hash mismatch"
            );
        } else {
            let response = self
                .request(|| Ok(self.client.head(&asset.browser_download_url).send()?))?
                .error_for_status()?;
            anyhow::ensure!(
                response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    == Some(size),
                "asset HEAD length mismatch"
            );
        }
        Ok(())
    }
    pub fn recover_pack(
        &self,
        owner: &str,
        repo: &str,
        tag: &str,
        pack: &vesper_format::PackEntry,
        destination: Option<&Path>,
    ) -> Result<()> {
        self.recover_asset(
            owner,
            repo,
            tag,
            &pack.pack_id,
            &r2::ObjectIdentity {
                size: pack.size,
                blake3: hex::encode(pack.hash),
            },
            destination,
        )
    }

    pub fn new(token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if token.trim().is_empty() {
            anyhow::bail!("GitHub token is empty");
        }
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("vesper/", env!("CARGO_PKG_VERSION")))
                .pool_max_idle_per_host(0)
                .connect_timeout(Duration::from_secs(30))
                .timeout(Duration::from_secs(300))
                .build()?,
            api_base: "https://api.github.com".into(),
            upload_base: "https://uploads.github.com".into(),
            token,
        })
    }

    pub fn with_endpoints(
        mut self,
        api_base: impl Into<String>,
        upload_base: impl Into<String>,
    ) -> Self {
        self.api_base = api_base.into().trim_end_matches('/').to_string();
        self.upload_base = upload_base.into().trim_end_matches('/').to_string();
        self
    }

    pub fn ensure_release(&self, owner: &str, repo: &str, tag: &str) -> Result<Release> {
        self.ensure_release_with(owner, repo, tag, true)
    }

    /// Mutable metadata releases pass `latest = false` so the repository's
    /// "Latest" release keeps pointing at a game build.
    fn ensure_release_with(
        &self,
        owner: &str,
        repo: &str,
        tag: &str,
        latest: bool,
    ) -> Result<Release> {
        if let Some(release) = self.get_release(owner, repo, tag)? {
            return Ok(release);
        }
        let url = format!("{}/repos/{}/{}/releases", self.api_base, owner, repo);
        let mut body = serde_json::json!({ "tag_name": tag, "name": tag, "draft": false, "prerelease": false });
        if !latest {
            body["make_latest"] = serde_json::json!("false");
        }
        let response = self.request(|| {
            Ok(self
                .request_builder(self.client.post(&url))
                .json(&body)
                .send()?)
        })?;
        Ok(response.error_for_status()?.json()?)
    }

    fn get_release(&self, owner: &str, repo: &str, tag: &str) -> Result<Option<Release>> {
        let url = format!(
            "{}/repos/{}/{}/releases/tags/{}",
            self.api_base,
            owner,
            repo,
            urlencoding::encode(tag)
        );
        let response =
            self.request(|| Ok(self.request_builder(self.client.get(&url)).send()?))?;
        if response.status().is_success() {
            return Ok(Some(response.json()?));
        }
        if response.status().as_u16() != 404 {
            return Err(response.error_for_status().unwrap_err().into());
        }
        Ok(None)
    }

    pub fn list_assets(&self, release: &Release) -> Result<Vec<ReleaseAsset>> {
        let response = self.request(|| {
            Ok(self
                .request_builder(self.client.get(&release.assets_url))
                .send()?)
        })?;
        Ok(response.error_for_status()?.json()?)
    }

    /// Replace the mutable developer metadata assets, publishing targets last.
    /// Readers can observe a mixed generation while uploads run; signed store
    /// bindings reject that generation and retain the last valid catalogue.
    pub fn publish_metadata(&self, owner: &str, repo: &str, tag: &str, dir: &Path) -> Result<()> {
        // The store files the targets sign (store.json, descriptions, changelogs), then targets.
        let targets: vesper_meta::Signed<vesper_meta::AppTargetsSigned> =
            vesper_meta::read_json(&dir.join("targets.json"))?;
        let mut files: Vec<String> = targets.signed.metadata.keys().cloned().collect();
        files.push("targets.json".into());
        for name in &files {
            anyhow::ensure!(dir.join(name).is_file(), "missing metadata file {name}");
        }
        let release = self.ensure_release_with(owner, repo, tag, false)?;
        let assets = self.list_assets(&release)?;
        // Store files that are no longer signed would only confuse readers.
        for asset in assets.iter().filter(|asset| {
            vesper_meta::store::is_store_file_name(&asset.name) && !files.contains(&asset.name)
        }) {
            let url = format!(
                "{}/repos/{owner}/{repo}/releases/assets/{}",
                self.api_base, asset.id
            );
            self.request(|| Ok(self.request_builder(self.client.delete(&url)).send()?))?
                .error_for_status()?;
        }
        for name in &files {
            let name = name.as_str();
            for asset in assets.iter().filter(|asset| asset.name == name) {
                let url = format!(
                    "{}/repos/{owner}/{repo}/releases/assets/{}",
                    self.api_base, asset.id
                );
                self.request(|| Ok(self.request_builder(self.client.delete(&url)).send()?))?
                    .error_for_status()?;
            }
            self.upload_asset(&release, name, &dir.join(name))?;
        }
        Ok(())
    }

    pub fn publish_manifest_and_packs(
        &self,
        owner: &str,
        repo: &str,
        tag: &str,
        manifest_path: &Path,
        packs_dir: &Path,
        manifest: &Manifest,
    ) -> Result<PublishResult> {
        if !manifest_path.is_file() {
            anyhow::bail!("manifest is missing: {}", manifest_path.display());
        }
        let mut files = vec![(
            manifest_path
                .file_name()
                .context("manifest filename missing")?
                .to_string_lossy()
                .into_owned(),
            manifest_path.to_path_buf(),
        )];
        let mut skipped_existing = Vec::new();
        for pack in &manifest.packs {
            let path = packs_dir.join(&pack.pack_id);
            if pack.release_tag.as_deref().unwrap_or(tag) == tag {
                if !path.is_file() {
                    self.verify_pack_in_release(owner, repo, tag, &pack.pack_id, pack.size)
                        .with_context(|| {
                            format!(
                                "manifest pack is missing locally and unavailable remotely: {}",
                                path.display()
                            )
                        })?;
                    skipped_existing.push(pack.pack_id.clone());
                    continue;
                }
                let size = fs::metadata(&path)
                    .with_context(|| format!("manifest pack is missing: {}", path.display()))?
                    .len();
                if size != pack.size {
                    anyhow::bail!(
                        "manifest pack {} has size {}, expected {}",
                        path.display(),
                        size,
                        pack.size
                    );
                }
                files.push((pack.pack_id.clone(), path));
            } else {
                let release_tag = pack.release_tag.as_deref().unwrap_or(tag);
                self.verify_pack_in_release(owner, repo, release_tag, &pack.pack_id, pack.size)?;
            }
        }
        let release = self.ensure_release(owner, repo, tag)?;
        let existing = self.list_assets(&release)?;
        let mut uploaded = Vec::new();
        for (name, path) in files {
            if let Some(asset) = existing.iter().find(|asset| asset.name == name) {
                let size = fs::metadata(&path)?.len();
                if asset.size != size {
                    anyhow::bail!(
                        "release asset {} exists with size {}, local size {}",
                        name,
                        asset.size,
                        size
                    );
                }
                skipped_existing.push(name);
                continue;
            }
            self.upload_asset(&release, &name, &path)?;
            uploaded.push(name);
        }
        // Re-list after uploads so the API-provided download URLs are used for
        // the same head/tail verification a client will perform.
        let assets = self.list_assets(&release)?;
        for asset in assets {
            if uploaded.iter().any(|name| name == &asset.name) && asset.name.ends_with(".vpack") {
                let local = if asset.name
                    == manifest_path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .as_ref()
                {
                    manifest_path.to_path_buf()
                } else {
                    packs_dir.join(&asset.name)
                };
                self.verify_asset_edges(&asset, &local)?;
            }
        }
        Ok(PublishResult {
            release,
            uploaded,
            skipped_existing,
        })
    }

    fn verify_pack_in_release(
        &self,
        owner: &str,
        repo: &str,
        release_tag: &str,
        pack_id: &str,
        expected_size: u64,
    ) -> Result<()> {
        // Release metadata is read with the publisher's token, so repeated publishes
        // do not exhaust the anonymous API limit (60 requests an hour). The download
        // check below stays anonymous: players must be able to fetch the pack.
        let url = format!(
            "{}/repos/{owner}/{repo}/releases/tags/{}",
            self.api_base,
            urlencoding::encode(release_tag)
        );
        let response =
            self.request(|| Ok(self.request_builder(self.client.get(&url)).send()?))?;
        if response.status().as_u16() == 404 {
            anyhow::bail!("pack {pack_id} release {release_tag} is missing");
        }
        let release: Release = response.error_for_status()?.json()?;
        let response = self.request(|| {
            Ok(self
                .request_builder(self.client.get(&release.assets_url))
                .send()?)
        })?;
        let assets: Vec<ReleaseAsset> = response.error_for_status()?.json()?;
        let asset = assets
            .iter()
            .find(|asset| asset.name == pack_id)
            .with_context(|| format!("pack {pack_id} is missing from release {release_tag}"))?;
        if asset.size != expected_size {
            anyhow::bail!("pack {pack_id} release {release_tag} asset size mismatch: got {}, expected {expected_size}", asset.size);
        }
        let response = self.request(|| {
            Ok(self
                .client
                .get(&asset.browser_download_url)
                .header(RANGE, "bytes=0-0")
                .send()?)
        })?;
        let status = response.status().as_u16();
        if status != 200 && status != 206 {
            anyhow::bail!(
                "pack {pack_id} release {release_tag} download check returned {}",
                response.status()
            );
        }
        let total = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit_once('/')?.1.parse::<u64>().ok())
            .or_else(|| {
                if status == 200 {
                    response
                        .headers()
                        .get(CONTENT_LENGTH)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                } else {
                    None
                }
            });
        if total != Some(expected_size) {
            anyhow::bail!(
                "pack {pack_id} release {release_tag} download size mismatch: got {:?}, expected {}",
                total,
                expected_size
            );
        }
        Ok(())
    }

    fn upload_asset(&self, release: &Release, name: &str, path: &Path) -> Result<()> {
        let bytes = fs::metadata(path)?.len();
        let mut url = release.upload_url.replace("{?name,label}", "");
        if !url.starts_with("http") {
            url = format!("{}{}", self.upload_base, url);
        }
        url.push_str("?name=");
        url.push_str(&urlencoding::encode(name));
        for attempt in 0..4 {
            if attempt > 0 {
                // A disconnected upload can leave a starter or complete asset behind.
                for asset in self
                    .list_assets(release)?
                    .into_iter()
                    .filter(|a| a.name == name)
                {
                    let url = format!(
                        "{}/releases/assets/{}",
                        release
                            .assets_url
                            .split("/releases/")
                            .next()
                            .filter(|base| *base != release.assets_url)
                            .unwrap_or(&self.api_base),
                        asset.id
                    );
                    self.request(|| Ok(self.request_builder(self.client.delete(&url)).send()?))?
                        .error_for_status()?;
                }
            }
            let result = transfer::runtime()?.block_on(async {
                let request = transfer::client()?
                    .post(&url)
                    .header(AUTHORIZATION, format!("Bearer {}", self.token))
                    .header(ACCEPT, "application/vnd.github+json")
                    .header(USER_AGENT, concat!("vesper/", env!("CARGO_PKG_VERSION")))
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header(CONTENT_LENGTH, bytes);
                transfer::send_file(request, Some(path), transfer::IDLE).await
            });
            match result {
                Ok((status, _)) if status.is_success() => return Ok(()),
                Ok((status, _)) if !r2::retry_status(status.as_u16()) =>
                    anyhow::bail!("upload GitHub asset {name} failed (HTTP {})", status.as_u16()),
                _ if attempt < 3 => thread::sleep(Duration::from_millis(250 << attempt)),
                _ => anyhow::bail!("upload GitHub asset {name} failed after retries (transport or retryable HTTP status)"),
            }
        }

        Ok(())
    }

    fn verify_asset_edges(&self, asset: &ReleaseAsset, local: &Path) -> Result<()> {
        let size = fs::metadata(local)?.len();
        if size == 0 {
            return Ok(());
        }
        let edge = 64 * 1024u64;
        let head_len = edge.min(size);
        let tail_start = size.saturating_sub(edge);
        let expected_head = read_edge(local, 0, head_len)?;
        let expected_tail = read_edge(local, tail_start, size - tail_start)?;
        let head = self
            .request(|| {
                Ok(self
                    .client
                    .get(&asset.browser_download_url)
                    .header(RANGE, format!("bytes=0-{}", head_len - 1))
                    .send()?)
            })?
            .error_for_status()?
            .bytes()?
            .to_vec();
        let tail = self
            .request(|| {
                Ok(self
                    .client
                    .get(&asset.browser_download_url)
                    .header(RANGE, format!("bytes={tail_start}-{}", size - 1))
                    .send()?)
            })?
            .error_for_status()?
            .bytes()?
            .to_vec();
        if head != expected_head || tail != expected_tail {
            anyhow::bail!(
                "downloaded asset edges do not match local {}",
                local.display()
            );
        }
        Ok(())
    }

    fn request<F>(&self, mut send: F) -> Result<Response>
    where
        F: FnMut() -> Result<Response>,
    {
        for attempt in 0..4 {
            match send() {
                Ok(response) if !r2::retry_status(response.status().as_u16()) => {
                    return Ok(response)
                }
                Err(error) if error.downcast_ref::<reqwest::Error>().is_none() => {
                    return Err(error)
                }
                _ if attempt < 3 => thread::sleep(Duration::from_millis(250 << attempt)),
                _ => anyhow::bail!(
                    "GitHub request failed after retries (transport or retryable HTTP status)"
                ),
            }
        }
        unreachable!()
    }
}

fn read_edge(path: &Path, start: u64, len: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0u8; len as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

impl GitHubPublisher {
    fn request_builder(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        request
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, concat!("vesper/", env!("CARGO_PKG_VERSION")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::sync::{Arc, Mutex};
    use vesper_format::{ChunkingParams, Manifest, PackEntry};

    struct GitHubMock {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl GitHubMock {
        fn start(files: Arc<Mutex<std::collections::BTreeMap<String, Vec<u8>>>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reqs = requests.clone();
            let halt = stop.clone();
            let base_thread = base.clone();
            let thread = std::thread::spawn(move || {
                while !halt.load(std::sync::atomic::Ordering::Relaxed) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    let _ = stream.set_nonblocking(false);
                    let mut request = Vec::with_capacity(8192);
                    let mut buf = [0u8; 2048];
                    loop {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&buf[..n]);
                        let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n")
                        else {
                            continue;
                        };
                        let content_length = String::from_utf8_lossy(&request[..header_end])
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= header_end + 4 + content_length {
                            break;
                        }
                    }
                    if request.is_empty() {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&request).to_string();
                    let path = text
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    reqs.lock().unwrap().push(path.clone());
                    let method = text
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    let authorized = text
                        .lines()
                        .any(|line| line.to_ascii_lowercase().starts_with("authorization:"));
                    if method == "GET"
                        && (path.starts_with("/repos/") || path == "/assets")
                        && !authorized
                    {
                        // API reads must carry the token (the anonymous limit is 60 an hour).
                        let _ = write!(
                            stream,
                            "HTTP/1.1 403 Forbidden
Content-Length: 0
Connection: close

"
                        );
                    } else if method == "GET" && path == "/repos/o/r/releases/tags/v0" {
                        let body = format!(
                            r#"{{"id":0,"tag_name":"v0","upload_url":"{}/upload{{?name,label}}","assets_url":"{}/assets"}}"#,
                            base_thread, base_thread
                        );
                        let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body);
                    } else if method == "GET" && path.starts_with("/repos/o/r/releases/tags/") {
                        let _=write!(stream,"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    } else if method == "POST" && path == "/repos/o/r/releases" {
                        let body = format!(
                            r#"{{"id":1,"tag_name":"v1","upload_url":"{}/upload{{?name,label}}","assets_url":"{}/assets"}}"#,
                            base_thread, base_thread
                        );
                        let _=write!(stream,"HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body);
                    } else if method == "GET" && path == "/assets" {
                        let entries=files.lock().unwrap().iter().map(|(name,bytes)| format!(r#"{{"id":1,"name":"{}","size":{},"browser_download_url":"{}/download/{}"}}"#,name,bytes.len(),base_thread,name)).collect::<Vec<_>>().join(",");
                        let body = format!("[{}]", entries);
                        let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body);
                    } else if method == "POST" && path.starts_with("/upload") {
                        let name = path.split("name=").nth(1).unwrap_or("").to_string();
                        let name = urlencoding::decode(&name).unwrap().into_owned();
                        let content = text
                            .split("\r\n\r\n")
                            .nth(1)
                            .unwrap_or("")
                            .as_bytes()
                            .to_vec();
                        files.lock().unwrap().insert(name.clone(), content);
                        let body = format!(
                            r#"{{"id":1,"name":"{}","size":0,"browser_download_url":"{}/download/{}"}}"#,
                            name, base_thread, name
                        );
                        let _=write!(stream,"HTTP/1.1 201 Created\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body);
                    } else if (method == "GET" || method == "HEAD")
                        && path.starts_with("/download/")
                    {
                        let has_auth = text
                            .lines()
                            .any(|line| line.to_ascii_lowercase().starts_with("authorization:"));
                        if has_auth {
                            let _=write!(stream,"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        } else {
                            let name = path.trim_start_matches("/download/");
                            let _=write!(stream,"HTTP/1.1 302 Found\r\nLocation: {}/blob/{}?X-Amz-Expires=60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",base_thread,name);
                        }
                    } else if (method == "GET" || method == "HEAD") && path.starts_with("/blob/") {
                        let name = path
                            .trim_start_matches("/blob/")
                            .split('?')
                            .next()
                            .unwrap_or("");
                        let bytes = files.lock().unwrap().get(name).cloned().unwrap_or_default();
                        let range = text.lines().find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("range: bytes=")
                                .map(str::to_string)
                        });
                        let (start, end) = range
                            .map(|v| {
                                let mut s = v.split('-');
                                (
                                    s.next().unwrap().parse::<usize>().unwrap_or(0),
                                    s.next()
                                        .unwrap()
                                        .parse::<usize>()
                                        .unwrap_or(bytes.len().saturating_sub(1)),
                                )
                            })
                            .unwrap_or((0, bytes.len().saturating_sub(1)));
                        let end = end.min(bytes.len().saturating_sub(1));
                        let body = if bytes.is_empty() {
                            &[][..]
                        } else {
                            &bytes[start.min(end)..=end]
                        };
                        let _=write!(stream,"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",start,end,bytes.len(),body.len());
                        let _ = stream.write_all(body);
                    } else {
                        let _=write!(stream,"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    let _ = stream.flush();
                    let _ = stream.shutdown(Shutdown::Write);
                }
            });
            Self {
                base,
                requests,
                stop,
                thread: Some(thread),
            }
        }
    }
    impl Drop for GitHubMock {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn publish_github_mock_uploads_and_verifies_asset_edges() {
        let manifest_dir = tempfile::tempdir().unwrap();
        let packs = manifest_dir.path().join("packs");
        fs::create_dir_all(&packs).unwrap();
        let pack_name = "pack-test.vpack";
        let pack_bytes = vec![b'a'; 100];
        fs::write(packs.join(pack_name), &pack_bytes).unwrap();
        let mut manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "v1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
        );
        manifest.packs.push(PackEntry {
            pack_id: pack_name.into(),
            size: pack_bytes.len() as u64,
            hash: vesper_format::hash_bytes(&pack_bytes),
            release_tag: Some("v1".into()),
        });
        let manifest_path = manifest_dir.path().join("manifest.cbor.zst");
        fs::write(&manifest_path, manifest.encode().unwrap()).unwrap();
        let files = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let server = GitHubMock::start(files);
        std::thread::sleep(Duration::from_millis(20));
        let publisher = GitHubPublisher::new("token")
            .unwrap()
            .with_endpoints(server.base.clone(), server.base.clone());
        let result = publisher
            .publish_manifest_and_packs("o", "r", "v1", &manifest_path, &packs, &manifest)
            .unwrap();
        assert_eq!(result.uploaded.len(), 2);
        assert!(result
            .uploaded
            .iter()
            .any(|name| name == "manifest.cbor.zst"));
        assert!(result.uploaded.iter().any(|name| name == pack_name));
        let paths = server.requests.lock().unwrap().clone();
        assert!(paths.iter().any(|p| p.starts_with("/upload?name=")));
        assert!(paths.iter().any(|p| p.starts_with("/download/")));
        assert!(paths.iter().any(|p| p.starts_with("/blob/")));
    }

    #[test]
    fn publish_v2_uploads_only_new_pack_and_checks_old_release() {
        let manifest_dir = tempfile::tempdir().unwrap();
        let packs = manifest_dir.path().join("packs");
        fs::create_dir_all(&packs).unwrap();
        let old_name = "pack-old.vpack";
        let new_name = "pack-new.vpack";
        let new_bytes = vec![b'n'; 100];
        let manifest_path = manifest_dir.path().join("manifest.cbor.zst");
        fs::write(packs.join(new_name), &new_bytes).unwrap();
        let mut manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "v1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
        );
        manifest.packs.push(PackEntry {
            pack_id: old_name.into(),
            size: 3,
            hash: vesper_format::hash_bytes(b"old"),
            release_tag: Some("v0".into()),
        });
        manifest.packs.push(PackEntry {
            pack_id: new_name.into(),
            size: new_bytes.len() as u64,
            hash: vesper_format::hash_bytes(&new_bytes),
            release_tag: Some("v1".into()),
        });
        fs::write(&manifest_path, manifest.encode().unwrap()).unwrap();
        let files = Arc::new(Mutex::new(std::collections::BTreeMap::from([(
            old_name.to_string(),
            b"old".to_vec(),
        )])));
        let server = GitHubMock::start(files);
        std::thread::sleep(Duration::from_millis(20));
        let publisher = GitHubPublisher::new("token")
            .unwrap()
            .with_endpoints(server.base.clone(), server.base.clone());
        let result = publisher
            .publish_manifest_and_packs("o", "r", "v1", &manifest_path, &packs, &manifest)
            .unwrap();
        assert!(result
            .uploaded
            .iter()
            .any(|name| name == "manifest.cbor.zst"));
        assert!(result.uploaded.iter().any(|name| name == new_name));
        assert!(!result.uploaded.iter().any(|name| name == old_name));
    }

    #[test]
    fn missing_old_pack_fails_before_release_creation() {
        let manifest_dir = tempfile::tempdir().unwrap();
        let packs = manifest_dir.path().join("packs");
        fs::create_dir_all(&packs).unwrap();
        let manifest_path = manifest_dir.path().join("manifest.cbor.zst");
        let mut manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "v1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
        );
        manifest.packs.push(PackEntry {
            pack_id: "pack-missing.vpack".into(),
            size: 3,
            hash: vesper_format::hash_bytes(b"old"),
            release_tag: Some("v0".into()),
        });
        fs::write(&manifest_path, manifest.encode().unwrap()).unwrap();
        let files = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let server = GitHubMock::start(files);
        std::thread::sleep(Duration::from_millis(20));
        let publisher = GitHubPublisher::new("token")
            .unwrap()
            .with_endpoints(server.base.clone(), server.base.clone());
        let error = publisher
            .publish_manifest_and_packs("o", "r", "v1", &manifest_path, &packs, &manifest)
            .unwrap_err();
        assert!(error.to_string().contains("missing from release v0"));
        let paths = server.requests.lock().unwrap().clone();
        assert!(!paths.iter().any(|path| path == "/repos/o/r/releases"));
        assert!(!paths.iter().any(|path| path.starts_with("/upload")));
    }
}
