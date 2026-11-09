//! Minimal path-style S3 PUT/HEAD with AWS Signature Version 4.
use anyhow::{Context, Result};
use reqwest::{blocking::Client, Method, Url};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use vesper_format::{Manifest, PackEntry};

/// Non-secret mirror settings. The public base includes the key prefix.
#[derive(Clone, Default)]
pub struct R2Config {
    pub bucket: String,
    pub account_id: String,
    pub endpoint: Option<String>,
    pub prefix: String,
    pub public_base: String,
}

pub fn validate_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid mirror URL")?;
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1")),
        "mirror URL requires HTTPS (HTTP is allowed only for 127.0.0.1 tests)"
    );
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "mirror URL must not contain credentials, query or fragment"
    );
    Ok(url)
}

impl R2Config {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.bucket.is_empty() && !self.bucket.contains(['/', '\\']),
            "R2 bucket is required and must be a single path segment"
        );
        anyhow::ensure!(
            self.endpoint.is_some()
                || (!self.account_id.is_empty()
                    && self.account_id.bytes().all(|b| b.is_ascii_alphanumeric())),
            "R2 account id or endpoint is required"
        );
        validate_url(&self.endpoint())?;
        let public = validate_url(&self.public_base)?;
        if public.host_str().is_some_and(|h| h.ends_with(".r2.dev")) {
            eprintln!("warning: r2.dev public access is rate limited and unsuitable for production; use a custom domain");
        }
        Ok(())
    }
    fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}.r2.cloudflarestorage.com", self.account_id))
    }
    pub fn url_template(&self) -> String {
        format!(
            "{}/{{release}}/{{pack_id}}",
            self.public_base.trim_end_matches('/')
        )
    }
    pub fn key(&self, tag: &str, name: &str) -> Result<String> {
        let key = [self.prefix.trim_matches('/'), tag, name]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        anyhow::ensure!(
            !tag.is_empty()
                && !name.is_empty()
                && !key
                    .split('/')
                    .any(|p| p == "." || p == ".." || p.contains('\\')),
            "invalid mirror object key"
        );
        Ok(key)
    }
}

// Deliberately no Debug implementation: credentials must not enter diagnostics.
pub struct R2Publisher {
    config: R2Config,
    client: Client,
    access: String,
    secret: String,
}

impl R2Publisher {
    pub fn from_env(config: R2Config) -> Result<Self> {
        config.validate()?;
        let read = |name| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .with_context(|| format!("{name} is required for R2 publishing"))
        };
        Self::new(
            config,
            read("VESPER_R2_ACCESS_KEY_ID")?,
            read("VESPER_R2_SECRET_ACCESS_KEY")?,
        )
    }
    fn new(config: R2Config, access: String, secret: String) -> Result<Self> {
        Ok(Self {
            config,
            access,
            secret,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(300))
                .build()?,
        })
    }
    fn request(
        &self,
        method: Method,
        key: &str,
        bytes: &[u8],
    ) -> Result<reqwest::blocking::Response> {
        let mut url = validate_url(&self.config.endpoint())?;
        let path = format!(
            "{}/{}/{}",
            url.path().trim_end_matches('/'),
            urlencoding::encode(&self.config.bucket),
            key.split('/')
                .map(|s| urlencoding::encode(s))
                .collect::<Vec<_>>()
                .join("/")
        );
        url.set_path(&path);
        let date = timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
        let hash = hex::encode(Sha256::digest(bytes));
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap()),
            None => url.host_str().unwrap().to_string(),
        };
        let headers = BTreeMap::from([
            ("host".to_string(), host),
            ("x-amz-content-sha256".into(), hash.clone()),
            ("x-amz-date".into(), date.clone()),
        ]);
        let authorization = authorization(
            &self.access,
            &self.secret,
            "auto",
            method.as_str(),
            url.path(),
            &date,
            &hash,
            &headers,
        );
        let mut request = self
            .client
            .request(method.clone(), url)
            .header("Authorization", authorization);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if method == Method::PUT {
            request = request.body(bytes.to_vec());
        }
        // S3 bodies and HTTP diagnostics are untrusted and can echo credentials.
        request
            .send()
            .map_err(|_| anyhow::anyhow!("R2 request transport failed"))
    }
    pub fn head(&self, key: &str) -> Result<Option<u64>> {
        let response = self.request(Method::HEAD, key, &[])?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        anyhow::ensure!(
            response.status().is_success(),
            "R2 HEAD failed (HTTP {})",
            response.status().as_u16()
        );
        Ok(Some(
            response
                .headers()
                .get("content-length")
                .context("R2 HEAD missing Content-Length")?
                .to_str()?
                .parse()?,
        ))
    }
    pub fn upload(&self, key: &str, bytes: &[u8], dry_run: bool) -> Result<()> {
        let skip = self.head(key)? == Some(bytes.len() as u64);
        println!(
            "r2 {} {key} bytes={}",
            if skip {
                "skipped"
            } else if dry_run {
                "would-upload"
            } else {
                "upload"
            },
            bytes.len()
        );
        if !skip && !dry_run {
            let response = self.request(Method::PUT, key, bytes)?;
            anyhow::ensure!(
                response.status().is_success(),
                "R2 PUT failed (HTTP {})",
                response.status().as_u16()
            );
        }
        if !dry_run {
            anyhow::ensure!(
                self.head(key)? == Some(bytes.len() as u64),
                "R2 object confirmation failed"
            );
        }
        Ok(())
    }
    /// Every referenced release is mirrored before metadata may be signed.
    pub fn mirror(
        &self,
        tag: &str,
        manifest_path: &Path,
        packs_dir: &Path,
        manifest: &Manifest,
        mut recover: impl FnMut(&str, &PackEntry) -> Result<Vec<u8>>,
        dry_run: bool,
    ) -> Result<()> {
        for pack in &manifest.packs {
            let release = pack.release_tag.as_deref().unwrap_or(tag);
            let key = self.config.key(release, &pack.pack_id)?;
            // Existing same-size objects satisfy the mirror contract without a download.
            if self.head(&key)? == Some(pack.size) {
                println!("r2 skipped {key} bytes={}", pack.size);
                continue;
            }
            let path = packs_dir.join(&pack.pack_id);
            let bytes = if path.is_file() {
                fs::read(path)?
            } else {
                recover(release, pack)?
            };
            verify_pack(pack, &bytes)?;
            self.upload(&key, &bytes, dry_run)?;
        }
        let name = manifest_path
            .file_name()
            .context("manifest filename missing")?
            .to_str()
            .context("manifest filename must be UTF-8")?;
        self.upload(
            &self.config.key(tag, name)?,
            &fs::read(manifest_path)?,
            dry_run,
        )?;
        Ok(())
    }
}

pub fn verify_pack(pack: &PackEntry, bytes: &[u8]) -> Result<()> {
    anyhow::ensure!(
        bytes.len() as u64 == pack.size && vesper_format::hash_bytes(bytes) == pack.hash,
        "mirror pack length/hash mismatch: {}",
        pack.pack_id
    );
    Ok(())
}

fn hmac(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut block = [0u8; 64];
    let key = if key.len() > 64 {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    block[..key.len()].copy_from_slice(&key);
    let inner: Vec<u8> = block
        .iter()
        .map(|b| b ^ 0x36)
        .chain(message.iter().copied())
        .collect();
    Sha256::digest(
        block
            .iter()
            .map(|b| b ^ 0x5c)
            .chain(Sha256::digest(inner))
            .collect::<Vec<_>>(),
    )
    .to_vec()
}

#[allow(clippy::too_many_arguments)]
fn authorization(
    access: &str,
    secret: &str,
    region: &str,
    method: &str,
    path: &str,
    date: &str,
    hash: &str,
    headers: &BTreeMap<String, String>,
) -> String {
    let names = headers.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical_headers = headers
        .iter()
        .map(|(k, v)| {
            format!(
                "{k}:{}\n",
                v.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        })
        .collect::<String>();
    let canonical = format!("{method}\n{path}\n\n{canonical_headers}\n{names}\n{hash}");
    let scope = format!("{}/{region}/s3/aws4_request", &date[..8]);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical))
    );
    let key = hmac(format!("AWS4{secret}").as_bytes(), &date.as_bytes()[..8]);
    let key = hmac(
        &hmac(&hmac(&key, region.as_bytes()), b"s3"),
        b"aws4_request",
    );
    format!(
        "AWS4-HMAC-SHA256 Credential={access}/{scope},SignedHeaders={names},Signature={}",
        hex::encode(hmac(&key, to_sign.as_bytes()))
    )
}

fn timestamp(seconds: u64) -> String {
    let z = (seconds / 86400) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread,
    };

    #[test]
    fn aws_s3_get_object_sigv4_vector() {
        // Published AWS example, not a real credential:
        // https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sig-v4-header-based-auth.html
        let hash = hex::encode(Sha256::digest([]));
        let headers = BTreeMap::from([
            ("host".into(), "examplebucket.s3.amazonaws.com".into()),
            ("range".into(), "bytes=0-9".into()),
            ("x-amz-content-sha256".into(), hash.clone()),
            ("x-amz-date".into(), "20130524T000000Z".into()),
        ]);
        let actual = authorization(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "GET",
            "/test.txt",
            "20130524T000000Z",
            &hash,
            &headers,
        );
        assert_eq!(actual, "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
        assert_eq!(timestamp(1369353600), "20130524T000000Z");
    }

    #[test]
    fn mock_s3_verifies_signature_head_skip_overwrite_and_dry_run() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let objects = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let puts = Arc::new(Mutex::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (data, uploads, halt) = (objects.clone(), puts.clone(), stop.clone());
        let server = thread::spawn(move || {
            while !halt.load(Ordering::Acquire) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 8192];
                let end = loop {
                    let n = socket.read(&mut buf).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let raw = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let mut line = raw.lines().next().unwrap().split_whitespace();
                let method = line.next().unwrap();
                let path = line.next().unwrap();
                let headers: BTreeMap<String, String> = raw
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().into()))
                    .collect();
                let len = headers
                    .get("content-length")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                while bytes.len() < end + len {
                    let n = socket.read(&mut buf).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                }
                let body = &bytes[end..end + len];
                let hash = hex::encode(Sha256::digest(body));
                assert_eq!(headers["x-amz-content-sha256"], hash);
                let signed = ["host", "x-amz-content-sha256", "x-amz-date"]
                    .into_iter()
                    .map(|k| (k.to_string(), headers[k].clone()))
                    .collect();
                assert_eq!(
                    headers["authorization"],
                    authorization(
                        "mock-access",
                        "mock-secret",
                        "auto",
                        method,
                        path,
                        &headers["x-amz-date"],
                        &hash,
                        &signed
                    )
                );
                let (status, length) = if method == "PUT" {
                    data.lock().unwrap().insert(path.into(), body.to_vec());
                    *uploads.lock().unwrap() += 1;
                    (200, 0)
                } else if let Some(value) = data.lock().unwrap().get(path) {
                    (200, value.len())
                } else {
                    (404, 0)
                };
                write!(socket, "HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        let config = R2Config {
            bucket: "bucket".into(),
            endpoint: Some(endpoint),
            public_base: "https://cdn.example.com/game".into(),
            prefix: "game".into(),
            ..Default::default()
        };
        let publisher =
            R2Publisher::new(config, "mock-access".into(), "mock-secret".into()).unwrap();
        let key = publisher.config.key("v 1", "pack+file").unwrap();
        publisher.upload(&key, b"abc", false).unwrap();
        publisher.upload(&key, b"xyz", false).unwrap();
        assert_eq!(*puts.lock().unwrap(), 1);
        publisher.upload(&key, b"longer", false).unwrap();
        publisher.upload("dry/new", b"new", true).unwrap();
        assert_eq!(*puts.lock().unwrap(), 2);
        assert_eq!(
            objects.lock().unwrap()["/bucket/game/v%201/pack%2Bfile"],
            b"longer"
        );
        stop.store(true, Ordering::Release);
        server.join().unwrap();
    }

    #[test]
    fn mirror_settings_reject_unsafe_urls_and_keys() {
        for url in [
            "http://cdn.example.com",
            "https://user:pass@example.com",
            "https://example.com/?secret=value",
        ] {
            assert!(validate_url(url).is_err());
        }
        assert!(R2Config::default().key("..", "pack").is_err());
    }
}
