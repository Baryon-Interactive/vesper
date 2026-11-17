//! Vesper/1 content format: deterministic manifests and immutable pack frames.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce, Tag};
use anyhow::{Context, Result};
use fastcdc::v2020::FastCDC;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;

pub const MAGIC: &[u8; 4] = b"VSPK";
pub const FRAME_VERSION: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 56;
pub const FLAG_ENCRYPTED: u8 = 1;
pub const MANIFEST_MAGIC: &[u8; 4] = b"VSPM";
pub const DEFAULT_MIN_CHUNK: usize = 256 * 1024;
pub const DEFAULT_AVG_CHUNK: usize = 1024 * 1024;
pub const DEFAULT_MAX_CHUNK: usize = 4 * 1024 * 1024;
pub const MAX_MANIFEST_FILES: usize = 1_000_000;
pub const MAX_MANIFEST_CHUNKS: usize = 4_000_000;
pub const MAX_MANIFEST_PACKS: usize = 100_000;
pub const MAX_MANIFEST_DECOMPRESSED: usize = 64 * 1024 * 1024;
pub const MAX_FRAME_RAW_SIZE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct ChunkId(pub [u8; 32]);

impl ChunkId {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn from_keyed_bytes(key: &[u8; 32], bytes: &[u8]) -> Self {
        Self(*blake3::keyed_hash(key, bytes).as_bytes())
    }

    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Display for ChunkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Codec {
    Raw = 0,
    Zstd = 1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRef {
    pub id: ChunkId,
    pub raw_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub content_hash: [u8; 32],
    pub chunks: Vec<ChunkRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkLocation {
    pub pack_id: String,
    pub offset: u64,
    pub frame_len: u64,
    pub codec: Codec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackEntry {
    pub pack_id: String,
    pub size: u64,
    pub hash: [u8; 32],
    #[serde(default)]
    pub release_tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkingParams {
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRules {
    pub user_files: Vec<String>,
    pub runtimes: Vec<RuntimeDependency>,
    pub launch: Option<LaunchSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDependency {
    pub name: String,
    pub version: String,
    pub installer_sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub app_id: String,
    pub platform: String,
    pub version: String,
    pub created_at: String,
    pub key_id: u64,
    pub encrypted: bool,
    pub chunking: ChunkingParams,
    pub files: Vec<FileEntry>,
    pub chunks: BTreeMap<ChunkId, ChunkLocation>,
    pub packs: Vec<PackEntry>,
    pub install: InstallRules,
}

impl Manifest {
    pub fn new(
        app_id: String,
        platform: String,
        version: String,
        chunking: ChunkingParams,
    ) -> Self {
        Self {
            schema: "Vesper/1".to_string(),
            app_id,
            platform,
            version,
            created_at: chrono_like_now(),
            key_id: 0,
            encrypted: false,
            chunking,
            files: Vec::new(),
            chunks: BTreeMap::new(),
            packs: Vec::new(),
            install: InstallRules {
                user_files: Vec::new(),
                runtimes: Vec::new(),
                launch: None,
            },
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        encode_manifest_payload(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(MANIFEST_MAGIC) {
            anyhow::bail!("encrypted manifest requires DepotKey");
        }
        let manifest = decode_manifest_payload(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn encode_with_crypto(&self, crypto: &CryptoContext) -> Result<Vec<u8>> {
        let mut payload = encode_manifest_payload(self)?;
        let header = manifest_header(self, crypto.key_id)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
            &crypto.manifest_key(&self.version),
        ));
        let tag = cipher
            .encrypt_in_place_detached(Nonce::from_slice(&[0u8; 12]), &header, &mut payload)
            .map_err(|_| anyhow::anyhow!("manifest encryption failed"))?;
        payload.extend_from_slice(&tag);
        Ok([header, payload].concat())
    }

    pub fn decode_with_crypto(bytes: &[u8], crypto: &CryptoContext) -> Result<Self> {
        let envelope = parse_manifest_envelope(bytes)?;
        let ManifestEnvelope {
            header,
            app_id,
            version,
            key_id,
            mut payload,
        } = envelope;
        if key_id != crypto.key_id || app_id != crypto.app_id {
            anyhow::bail!("manifest key scope does not match supplied DepotKey");
        }
        if payload.len() < 16 {
            anyhow::bail!("encrypted manifest is missing auth tag");
        }
        let tag_bytes = payload.split_off(payload.len() - 16);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&crypto.manifest_key(&version)));
        cipher
            .decrypt_in_place_detached(
                Nonce::from_slice(&[0u8; 12]),
                &header,
                &mut payload,
                Tag::from_slice(&tag_bytes),
            )
            .map_err(|_| anyhow::anyhow!("manifest authentication failed"))?;
        let manifest = decode_manifest_payload(&payload)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn encrypted_header(bytes: &[u8]) -> Result<(String, String, u64)> {
        let envelope = parse_manifest_envelope(bytes)?;
        Ok((envelope.app_id, envelope.version, envelope.key_id))
    }

    pub fn sha256(&self) -> Result<[u8; 32]> {
        Ok(hash_bytes(&self.encode()?))
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != "Vesper/1" {
            anyhow::bail!("unsupported manifest schema {}", self.schema);
        }
        if self.encrypted && self.key_id == 0 {
            anyhow::bail!("encrypted manifest must declare a non-zero key_id");
        }
        validate_chunking(&self.chunking)?;
        if self.files.len() > MAX_MANIFEST_FILES {
            anyhow::bail!("manifest contains too many files");
        }
        if self.chunks.len() > MAX_MANIFEST_CHUNKS {
            anyhow::bail!("manifest contains too many chunks");
        }
        if self.packs.len() > MAX_MANIFEST_PACKS {
            anyhow::bail!("manifest contains too many packs");
        }
        let mut seen = std::collections::HashSet::new();
        let pack_ids: std::collections::HashSet<&str> = self
            .packs
            .iter()
            .map(|pack| pack.pack_id.as_str())
            .collect();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            let folded = file.path.to_lowercase();
            if !seen.insert(folded) {
                anyhow::bail!("case-insensitive path collision: {}", file.path);
            }
            for chunk in &file.chunks {
                if chunk.raw_size > MAX_FRAME_RAW_SIZE {
                    anyhow::bail!("chunk {} exceeds raw size limit", chunk.id);
                }
                let Some(location) = self.chunks.get(&chunk.id) else {
                    anyhow::bail!("file {} references missing chunk {}", file.path, chunk.id);
                };
                if location.frame_len == 0 || location.pack_id.is_empty() {
                    anyhow::bail!("invalid location for chunk {}", chunk.id);
                }
                if !pack_ids.contains(location.pack_id.as_str()) {
                    anyhow::bail!(
                        "chunk {} references unknown pack {}",
                        chunk.id,
                        location.pack_id
                    );
                }
            }
        }
        Ok(())
    }
}

fn encode_manifest_payload(manifest: &Manifest) -> Result<Vec<u8>> {
    let mut cbor = Vec::new();
    ciborium::ser::into_writer(manifest, &mut cbor).context("encode manifest CBOR")?;
    zstd_compress(&cbor, 8).context("compress manifest")
}

fn decode_manifest_payload(bytes: &[u8]) -> Result<Manifest> {
    let cbor = zstd_decompress_limited(bytes, MAX_MANIFEST_DECOMPRESSED as u64)
        .context("decompress manifest")?;
    if cbor.len() > MAX_MANIFEST_DECOMPRESSED {
        anyhow::bail!("manifest decompressed output exceeds limit");
    }
    ciborium::de::from_reader(cbor.as_slice()).context("decode manifest CBOR")
}

fn manifest_header(manifest: &Manifest, key_id: u64) -> Result<Vec<u8>> {
    let app = manifest.app_id.as_bytes();
    let version = manifest.version.as_bytes();
    if app.len() > u16::MAX as usize || version.len() > u16::MAX as usize {
        anyhow::bail!("manifest identity fields are too long");
    }
    let mut header = Vec::with_capacity(17 + app.len() + version.len());
    header.extend_from_slice(MANIFEST_MAGIC);
    header.push(1);
    header.extend_from_slice(&key_id.to_le_bytes());
    header.extend_from_slice(&(app.len() as u16).to_le_bytes());
    header.extend_from_slice(&(version.len() as u16).to_le_bytes());
    header.extend_from_slice(app);
    header.extend_from_slice(version);
    Ok(header)
}

struct ManifestEnvelope {
    header: Vec<u8>,
    app_id: String,
    version: String,
    key_id: u64,
    payload: Vec<u8>,
}

fn parse_manifest_envelope(bytes: &[u8]) -> Result<ManifestEnvelope> {
    if bytes.len() < 17 || &bytes[..4] != MANIFEST_MAGIC || bytes[4] != 1 {
        anyhow::bail!("invalid encrypted manifest envelope");
    }
    let key_id = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
    let app_len = u16::from_le_bytes(bytes[13..15].try_into().unwrap()) as usize;
    let version_len = u16::from_le_bytes(bytes[15..17].try_into().unwrap()) as usize;
    let header_len = 17usize.saturating_add(app_len).saturating_add(version_len);
    if bytes.len() < header_len {
        anyhow::bail!("truncated encrypted manifest envelope");
    }
    let app_start = 17;
    let version_start = app_start + app_len;
    let app_id = String::from_utf8(bytes[app_start..version_start].to_vec())
        .context("manifest app_id is not UTF-8")?;
    let version = String::from_utf8(bytes[version_start..header_len].to_vec())
        .context("manifest version is not UTF-8")?;
    Ok(ManifestEnvelope {
        header: bytes[..header_len].to_vec(),
        app_id,
        version,
        key_id,
        payload: bytes[header_len..].to_vec(),
    })
}

#[derive(Debug, Clone)]
pub struct PackFrame {
    pub flags: u8,
    pub codec: Codec,
    pub key_id: u64,
    pub stored_tag: [u8; 32],
    pub raw_size: u32,
    pub stored_size: u32,
    pub payload: Vec<u8>,
    pub auth_tag: Option<[u8; 16]>,
}

impl PackFrame {
    pub fn encoded_len(&self) -> u64 {
        FRAME_HEADER_LEN as u64
            + self.payload.len() as u64
            + if self.flags & FLAG_ENCRYPTED != 0 {
                16
            } else {
                0
            }
    }

    pub fn write_to<W: Write>(&self, mut out: W) -> Result<u64> {
        let header = self.header_bytes();
        out.write_all(&header)?;
        out.write_all(&self.payload)?;
        if self.flags & FLAG_ENCRYPTED != 0 {
            let tag = self
                .auth_tag
                .ok_or_else(|| anyhow::anyhow!("encrypted frame is missing auth tag"))?;
            out.write_all(&tag)?;
        }
        Ok(self.encoded_len())
    }

    pub fn read_from<R: Read>(mut input: R, max_raw_size: u64) -> Result<Self> {
        let mut fixed = [0u8; FRAME_HEADER_LEN];
        input.read_exact(&mut fixed).context("read frame header")?;
        if &fixed[..4] != MAGIC {
            anyhow::bail!("invalid pack frame magic");
        }
        if fixed[4] != FRAME_VERSION || fixed[7] != 0 || fixed[5] & !FLAG_ENCRYPTED != 0 {
            anyhow::bail!("unsupported frame version, flags, or reserved byte");
        }
        let codec = match fixed[6] {
            0 => Codec::Raw,
            1 => Codec::Zstd,
            other => anyhow::bail!("unknown frame codec {}", other),
        };
        let key_id = u64::from_le_bytes(fixed[8..16].try_into().unwrap());
        let raw_size = u32::from_le_bytes(fixed[16..20].try_into().unwrap());
        let stored_size = u32::from_le_bytes(fixed[20..24].try_into().unwrap());
        let stored_tag: [u8; 32] = fixed[24..56].try_into().unwrap();
        if u64::from(raw_size) > max_raw_size
            || u64::from(stored_size) > max_raw_size.saturating_mul(2)
        {
            anyhow::bail!("frame exceeds configured size limit");
        }
        let mut payload = vec![0u8; stored_size as usize];
        input
            .read_exact(&mut payload)
            .context("read frame payload")?;
        let auth_tag = if fixed[5] & FLAG_ENCRYPTED != 0 {
            let mut tag = [0u8; 16];
            input.read_exact(&mut tag).context("read frame auth tag")?;
            Some(tag)
        } else {
            None
        };
        Ok(Self {
            flags: fixed[5],
            codec,
            raw_size,
            stored_size,
            key_id,
            stored_tag,
            payload,
            auth_tag,
        })
    }

    pub fn decode_payload(&self, crypto: Option<&CryptoContext>) -> Result<Vec<u8>> {
        let mut stored = self.payload.clone();
        if self.flags & FLAG_ENCRYPTED != 0 {
            let crypto =
                crypto.ok_or_else(|| anyhow::anyhow!("encrypted frame requires DepotKey"))?;
            if crypto.key_id != self.key_id {
                anyhow::bail!(
                    "frame key_id {} does not match supplied key_id {}",
                    self.key_id,
                    crypto.key_id
                );
            }
            let tag = self
                .auth_tag
                .ok_or_else(|| anyhow::anyhow!("encrypted frame is missing auth tag"))?;
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
                &crypto.chunk_key(&self.stored_tag),
            ));
            cipher
                .decrypt_in_place_detached(
                    Nonce::from_slice(&[0u8; 12]),
                    &self.header_bytes(),
                    &mut stored,
                    Tag::from_slice(&tag),
                )
                .map_err(|_| anyhow::anyhow!("chunk authentication failed"))?;
            if blake3::keyed_hash(&crypto.manifest_key, &stored).as_bytes() != &self.stored_tag {
                anyhow::bail!("encrypted stored tag mismatch");
            }
        } else if blake3::hash(&stored).as_bytes() != &self.stored_tag {
            anyhow::bail!("stored tag mismatch");
        }
        let raw = match self.codec {
            Codec::Raw => stored,
            Codec::Zstd => {
                zstd_decompress_limited(&stored, u64::from(self.raw_size))
                    .context("decompress chunk")?
            }
        };
        if raw.len() as u32 != self.raw_size {
            anyhow::bail!(
                "raw size mismatch: expected {}, got {}",
                self.raw_size,
                raw.len()
            );
        }
        Ok(raw)
    }

    pub fn header_bytes(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut header = [0u8; FRAME_HEADER_LEN];
        header[..4].copy_from_slice(MAGIC);
        header[4] = FRAME_VERSION;
        header[5] = self.flags;
        header[6] = self.codec as u8;
        header[7] = 0;
        header[8..16].copy_from_slice(&self.key_id.to_le_bytes());
        header[16..20].copy_from_slice(&self.raw_size.to_le_bytes());
        header[20..24].copy_from_slice(&self.stored_size.to_le_bytes());
        header[24..56].copy_from_slice(&self.stored_tag);
        header
    }

    pub fn from_chunk(raw: &[u8], crypto: Option<&CryptoContext>) -> Result<(Self, ChunkId)> {
        let (codec, stored) = choose_codec(raw)?;
        let (chunk_id, flags, key_id, stored_tag, payload, auth_tag) = if let Some(crypto) = crypto
        {
            let chunk_id = crypto.chunk_id(raw);
            let stored_tag = crypto.stored_tag(&stored);
            let mut frame = Self {
                flags: FLAG_ENCRYPTED,
                codec,
                key_id: crypto.key_id,
                stored_tag,
                raw_size: raw.len().try_into().context("raw chunk exceeds u32")?,
                stored_size: stored
                    .len()
                    .try_into()
                    .context("stored chunk exceeds u32")?,
                payload: stored,
                auth_tag: None,
            };
            let cipher =
                Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&crypto.chunk_key(&stored_tag)));
            let tag = cipher
                .encrypt_in_place_detached(
                    Nonce::from_slice(&[0u8; 12]),
                    &frame.header_bytes(),
                    &mut frame.payload,
                )
                .map_err(|_| anyhow::anyhow!("chunk encryption failed"))?;
            (
                chunk_id,
                frame.flags,
                frame.key_id,
                frame.stored_tag,
                frame.payload,
                Some(tag.into()),
            )
        } else {
            (
                ChunkId::from_bytes(raw),
                0,
                0,
                *blake3::hash(&stored).as_bytes(),
                stored,
                None,
            )
        };
        Ok((
            Self {
                flags,
                codec,
                key_id,
                stored_tag,
                raw_size: raw.len().try_into().context("raw chunk exceeds u32")?,
                stored_size: payload
                    .len()
                    .try_into()
                    .context("stored chunk exceeds u32")?,
                payload,
                auth_tag,
            },
            chunk_id,
        ))
    }
}

#[derive(Clone)]
pub struct CryptoContext {
    depot_key: [u8; 32],
    pub key_id: u64,
    app_id: String,
    manifest_key: [u8; 32],
}

impl CryptoContext {
    pub fn new(depot_key: [u8; 32], key_id: u64, app_id: impl Into<String>) -> Result<Self> {
        if key_id == 0 {
            anyhow::bail!("key_id must be non-zero for encrypted builds");
        }
        let app_id = app_id.into();
        let mut info = b"VESPER1-MANIFEST".to_vec();
        info.extend_from_slice(&(app_id.len() as u64).to_le_bytes());
        info.extend_from_slice(app_id.as_bytes());
        let hk = Hkdf::<Sha256>::new(None, &depot_key);
        let mut manifest_key = [0u8; 32];
        hk.expand(&info, &mut manifest_key)
            .map_err(|_| anyhow::anyhow!("manifest key derivation failed"))?;
        Ok(Self {
            depot_key,
            key_id,
            app_id,
            manifest_key,
        })
    }

    pub fn from_hex(hex_key: &str, key_id: u64, app_id: impl Into<String>) -> Result<Self> {
        let bytes: [u8; 32] = hex::decode(hex_key.trim())
            .context("decode DepotKey")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("DepotKey must be 32 bytes"))?;
        Self::new(bytes, key_id, app_id)
    }

    pub fn chunk_id(&self, raw: &[u8]) -> ChunkId {
        ChunkId::from_keyed_bytes(&self.manifest_key, raw)
    }
    pub fn stored_tag(&self, stored: &[u8]) -> [u8; 32] {
        *blake3::keyed_hash(&self.manifest_key, stored).as_bytes()
    }

    fn manifest_key(&self, version: &str) -> [u8; 32] {
        let mut info = b"VESPER1-MANIFEST-ENC".to_vec();
        info.extend_from_slice(&(self.app_id.len() as u64).to_le_bytes());
        info.extend_from_slice(self.app_id.as_bytes());
        info.extend_from_slice(&(version.len() as u64).to_le_bytes());
        info.extend_from_slice(version.as_bytes());
        let hk = Hkdf::<Sha256>::new(None, &self.depot_key);
        let mut key = [0u8; 32];
        hk.expand(&info, &mut key).expect("fixed-size HKDF output");
        key
    }

    fn chunk_key(&self, stored_tag: &[u8; 32]) -> [u8; 32] {
        let mut info = b"VESPER1-CHUNK".to_vec();
        info.extend_from_slice(&(self.app_id.len() as u64).to_le_bytes());
        info.extend_from_slice(self.app_id.as_bytes());
        info.extend_from_slice(stored_tag);
        let hk = Hkdf::<Sha256>::new(Some(&self.key_id.to_le_bytes()), &self.depot_key);
        let mut key = [0u8; 32];
        hk.expand(&info, &mut key).expect("fixed-size HKDF output");
        key
    }
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("invalid path: {0}")]
    Invalid(String),
}

pub fn validate_relative_path(path: &str) -> Result<(), PathError> {
    if path.is_empty() || path.contains('\0') || path.starts_with('/') || path.starts_with('\\') {
        return Err(PathError::Invalid(path.to_string()));
    }
    let normalized = path.nfc().collect::<String>();
    if normalized != path
        || path.contains(':')
        || path.split('/').any(|part| part == ".." || part.is_empty())
    {
        return Err(PathError::Invalid(path.to_string()));
    }
    for component in path.split('/') {
        let stem = component.trim_end_matches([' ', '.']);
        let upper = stem.to_ascii_uppercase();
        let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (upper.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && upper.as_bytes()[3].is_ascii_digit()
                && upper.as_bytes()[3] != b'0');
        if reserved {
            return Err(PathError::Invalid(path.to_string()));
        }
    }
    Ok(())
}

pub fn validate_chunking(params: &ChunkingParams) -> Result<()> {
    if !((64..=1_048_576).contains(&params.min_size)
        && (256..=4_194_304).contains(&params.avg_size)
        && (1024..=16_777_216).contains(&params.max_size)
        && params.min_size < params.avg_size
        && params.avg_size < params.max_size)
    {
        anyhow::bail!(
            "invalid FastCDC parameters: min={}, avg={}, max={}",
            params.min_size,
            params.avg_size,
            params.max_size
        );
    }
    Ok(())
}

pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

pub fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Mode recorded for a packed file: 0o755 when any execute bit is set on Unix,
/// otherwise 0o644. Windows has no execute bit, so builds packed there record 0o644
/// and installers rely on the manifest's launch target instead.
pub fn packed_file_mode(path: &Path) -> Result<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = fs::metadata(path)?.permissions().mode() & 0o111 != 0;
        Ok(if executable { 0o755 } else { 0o644 })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(0o644)
    }
}

pub fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher).context("hash file")?;
    Ok(*hasher.finalize().as_bytes())
}

pub fn chunk_bytes<'a>(bytes: &'a [u8], params: &ChunkingParams) -> Vec<&'a [u8]> {
    if bytes.len() < params.min_size as usize {
        return vec![bytes];
    }
    FastCDC::new(bytes, params.min_size, params.avg_size, params.max_size)
        .map(|chunk| &bytes[chunk.offset..chunk.offset + chunk.length])
        .collect()
}

pub struct PackWriter {
    file: File,
    path: PathBuf,
    pub pack_id: String,
    pub size: u64,
    hasher: blake3::Hasher,
}

impl PackWriter {
    pub fn create(dir: &Path, ordinal: usize) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("pack-{ordinal:04}.vesper.part"));
        let file = File::create(&path)?;
        let pack_id = path.file_name().unwrap().to_string_lossy().into_owned();
        Ok(Self {
            file,
            path,
            pack_id,
            size: 0,
            hasher: blake3::Hasher::new(),
        })
    }

    pub fn append(&mut self, frame: &PackFrame) -> Result<(u64, u64)> {
        let offset = self.size;
        let mut bytes = Vec::with_capacity(frame.encoded_len() as usize);
        frame.write_to(&mut bytes)?;
        self.file.write_all(&bytes)?;
        self.hasher.update(&bytes);
        self.size += bytes.len() as u64;
        Ok((offset, bytes.len() as u64))
    }

    pub fn finish(mut self) -> Result<PackEntry> {
        self.file.flush()?;
        self.file.sync_all()?;
        let temporary_path = self.path.clone();
        // Close the file before renaming it (required on Windows). WebAssembly
        // builds never pack, and their File stub has no Drop.
        #[allow(clippy::drop_non_drop)]
        drop(self.file);
        let hash = *self.hasher.finalize().as_bytes();
        let final_name = format!("pack-{}.vpack", hex::encode(hash));
        let final_path = temporary_path.with_file_name(&final_name);
        fs::rename(&temporary_path, &final_path)?;
        Ok(PackEntry {
            pack_id: final_name,
            size: self.size,
            hash,
            release_tag: None,
        })
    }
}

pub struct BuildOutput {
    pub manifest: Manifest,
    pub packs: Vec<PathBuf>,
    pub manifest_path: PathBuf,
}

/// A previous manifest that may be searched for reusable content-addressed chunks.
#[derive(Debug, Clone, Copy)]
pub struct PreviousBuild<'a> {
    pub manifest: &'a Manifest,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    pub deletable_packs: Vec<String>,
    pub sparse_packs: Vec<String>,
}

/// Compute pack retention information without deleting anything.
pub fn gc_report(retained: &[Manifest], packs_dir: &Path) -> Result<GcReport> {
    let mut referenced = std::collections::BTreeSet::new();
    let mut live = BTreeMap::<String, u64>::new();
    for manifest in retained {
        for pack in &manifest.packs {
            referenced.insert(pack.pack_id.clone());
        }
        for location in manifest.chunks.values() {
            *live.entry(location.pack_id.clone()).or_default() += location.frame_len;
        }
    }
    let mut report = GcReport::default();
    if packs_dir.is_dir() {
        for entry in fs::read_dir(packs_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".vpack") {
                continue;
            }
            if !referenced.contains(&name) {
                report.deletable_packs.push(name);
                continue;
            }
            let size = entry.metadata()?.len();
            if size > 0 && live.get(&name).copied().unwrap_or_default() * 2 < size {
                report.sparse_packs.push(name);
            }
        }
    }
    report.deletable_packs.sort();
    report.sparse_packs.sort();
    Ok(report)
}

pub fn build_from_directory(
    input: &Path,
    output: &Path,
    app_id: String,
    platform: String,
    version: String,
    chunking: ChunkingParams,
    max_pack_size: u64,
) -> Result<BuildOutput> {
    build_from_directory_with_crypto(
        input,
        output,
        app_id,
        platform,
        version,
        chunking,
        max_pack_size,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_from_directory_with_crypto(
    input: &Path,
    output: &Path,
    app_id: String,
    platform: String,
    version: String,
    chunking: ChunkingParams,
    max_pack_size: u64,
    crypto: Option<CryptoContext>,
) -> Result<BuildOutput> {
    build_from_directory_with_previous(
        input,
        output,
        app_id,
        platform,
        version,
        chunking,
        max_pack_size,
        crypto,
        &[],
    )
}

/// Build a deterministic manifest while reusing chunks from one or more
/// previous manifests. The old pack bytes are not needed locally: the returned
/// manifest keeps each reused pack's id and release tag so a downloader can
/// locate it in its published release.
#[allow(clippy::too_many_arguments)]
pub fn build_from_directory_with_previous(
    input: &Path,
    output: &Path,
    app_id: String,
    platform: String,
    version: String,
    chunking: ChunkingParams,
    max_pack_size: u64,
    crypto: Option<CryptoContext>,
    previous: &[PreviousBuild<'_>],
) -> Result<BuildOutput> {
    validate_chunking(&chunking)?;
    fs::create_dir_all(output)?;
    let packs_dir = output.join("packs");
    fs::create_dir_all(&packs_dir)?;
    let mut manifest = Manifest::new(app_id, platform, version, chunking.clone());
    if let Some(crypto) = &crypto {
        manifest.key_id = crypto.key_id;
        manifest.encrypted = true;
    }
    let mut writer = PackWriter::create(&packs_dir, 0)?;
    let mut pack_ordinal = 0usize;
    let mut pack_paths = Vec::new();

    let mut reusable = BTreeMap::<ChunkId, (ChunkLocation, PackEntry)>::new();
    let mut missing_release_tags = BTreeMap::<ChunkId, String>::new();
    for old in previous {
        old.manifest.validate()?;
        let packs = old
            .manifest
            .packs
            .iter()
            .map(|pack| (pack.pack_id.as_str(), pack))
            .collect::<BTreeMap<_, _>>();
        for (id, location) in &old.manifest.chunks {
            let pack = packs
                .get(location.pack_id.as_str())
                .expect("manifest validation checked chunk pack references");
            if pack.release_tag.is_some() {
                reusable
                    .entry(*id)
                    .or_insert_with(|| (location.clone(), (**pack).clone()));
            } else {
                missing_release_tags
                    .entry(*id)
                    .or_insert_with(|| pack.pack_id.clone());
            }
        }
    }
    for (id, pack_id) in missing_release_tags {
        if !reusable.contains_key(&id) {
            anyhow::bail!(
                "previous pack {pack_id} is missing release_tag; add the published release tag to the previous manifest"
            );
        }
    }
    let mut paths = Vec::new();
    for entry in WalkDir::new(input).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            paths.push(entry.into_path());
        } else if entry.file_type().is_symlink() {
            anyhow::bail!(
                "symbolic links are not supported: {}",
                entry.path().display()
            );
        }
    }
    paths.sort();

    for path in paths {
        let relative = path
            .strip_prefix(input)?
            .to_string_lossy()
            .replace('\\', "/")
            .nfc()
            .collect::<String>();
        validate_relative_path(&relative)?;
        let bytes = fs::read(&path)?;
        let mut file = FileEntry {
            path: relative,
            size: bytes.len() as u64,
            mode: packed_file_mode(&path)?,
            content_hash: hash_file(&path)?,
            chunks: Vec::new(),
        };
        for chunk in chunk_bytes(&bytes, &chunking) {
            let (frame, id) = PackFrame::from_chunk(chunk, crypto.as_ref())?;
            if let Some((old_location, old_pack)) = reusable.get(&id).cloned() {
                if let std::collections::btree_map::Entry::Vacant(entry) = manifest.chunks.entry(id)
                {
                    entry.insert(old_location);
                    if !manifest.packs.iter().any(|p| p.pack_id == old_pack.pack_id) {
                        manifest.packs.push(old_pack);
                    }
                }
                file.chunks.push(ChunkRef {
                    id,
                    raw_size: chunk.len() as u64,
                });
                continue;
            }
            if writer.size > 0 && writer.size + frame.encoded_len() > max_pack_size {
                let temporary_id = writer.pack_id.clone();
                let finished = writer.finish()?;
                for location in manifest.chunks.values_mut() {
                    if location.pack_id == temporary_id {
                        location.pack_id = finished.pack_id.clone();
                    }
                }
                pack_paths.push(packs_dir.join(&finished.pack_id));
                manifest.packs.push(finished);
                pack_ordinal += 1;
                writer = PackWriter::create(&packs_dir, pack_ordinal)?;
            }
            let (offset, frame_len) = if let Some(existing) = manifest.chunks.get(&id) {
                (existing.offset, existing.frame_len)
            } else {
                let (offset, frame_len) = writer.append(&frame)?;
                manifest.chunks.insert(
                    id,
                    ChunkLocation {
                        pack_id: writer.pack_id.clone(),
                        offset,
                        frame_len,
                        codec: frame.codec,
                    },
                );
                (offset, frame_len)
            };
            let _ = (offset, frame_len);
            file.chunks.push(ChunkRef {
                id,
                raw_size: chunk.len() as u64,
            });
        }
        manifest.files.push(file);
    }

    if writer.size > 0 {
        let temporary_id = writer.pack_id.clone();
        let finished = writer.finish()?;
        for location in manifest.chunks.values_mut() {
            if location.pack_id == temporary_id {
                location.pack_id = finished.pack_id.clone();
            }
        }
        pack_paths.push(packs_dir.join(&finished.pack_id));
        manifest.packs.push(finished);
    } else {
        let empty = writer.path.clone();
        drop(writer);
        let _ = fs::remove_file(empty);
    }
    manifest.validate()?;
    let manifest_path = output.join("manifest.cbor.zst");
    fs::write(&manifest_path, manifest.encode()?)?;
    Ok(BuildOutput {
        manifest,
        packs: pack_paths,
        manifest_path,
    })
}

fn choose_codec(raw: &[u8]) -> Result<(Codec, Vec<u8>)> {
    let compressed = zstd_compress(raw, 3)?;
    if compressed.len().saturating_mul(100) < raw.len().saturating_mul(98) {
        Ok((Codec::Zstd, compressed))
    } else {
        Ok((Codec::Raw, raw.to_vec()))
    }
}

/// Compress with the C Zstandard library. WebAssembly builds link no C code and
/// only decode, so packing is unavailable there.
#[cfg(not(target_arch = "wasm32"))]
fn zstd_compress(bytes: &[u8], level: i32) -> Result<Vec<u8>> {
    Ok(zstd::stream::encode_all(bytes, level)?)
}

#[cfg(target_arch = "wasm32")]
fn zstd_compress(_bytes: &[u8], _level: i32) -> Result<Vec<u8>> {
    anyhow::bail!("packing is not available in WebAssembly builds")
}

/// Decompress a Zstandard stream, reading at most `limit + 1` bytes so callers
/// can reject output larger than they expect without buffering all of it.
#[cfg(not(target_arch = "wasm32"))]
fn zstd_decompress_limited(bytes: &[u8], limit: u64) -> io::Result<Vec<u8>> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut raw = Vec::new();
    decoder.take(limit.saturating_add(1)).read_to_end(&mut raw)?;
    Ok(raw)
}

/// Pure-Rust decoder used in the browser, where the C library is not linked.
#[cfg(target_arch = "wasm32")]
fn zstd_decompress_limited(bytes: &[u8], limit: u64) -> io::Result<Vec<u8>> {
    let decoder = ruzstd::decoding::StreamingDecoder::new(bytes).map_err(io::Error::other)?;
    let mut raw = Vec::new();
    decoder.take(limit.saturating_add(1)).read_to_end(&mut raw)?;
    Ok(raw)
}

fn chrono_like_now() -> String {
    // Reproducible builds use SOURCE_DATE_EPOCH.  Falling back to zero keeps
    // the pack and manifest byte-identical for the same input and parameters.
    // A publisher that wants wall-clock freshness can set the environment
    // variable in its CI job.
    let secs = std::env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "0".to_string());
    format!("unix:{secs}")
}

pub fn read_frame_at(path: &Path, offset: u64, frame_len: u64) -> Result<PackFrame> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let frame = PackFrame::read_from(&mut file, MAX_FRAME_RAW_SIZE)?;
    if frame.encoded_len() != frame_len {
        anyhow::bail!("frame length mismatch at {}", offset);
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {

    #[cfg(unix)]
    #[test]
    fn pack_records_execute_bit_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let game = input.path().join("game");
        let data = input.path().join("data.bin");
        fs::write(
            &game,
            b"#!/bin/sh
echo game
",
        )
        .unwrap();
        fs::write(&data, b"payload").unwrap();
        fs::set_permissions(&game, fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o600)).unwrap();
        let built = build_from_directory(
            input.path(),
            output.path(),
            "demo".into(),
            "linux-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
            1024 * 1024,
        )
        .unwrap();
        let mode = |name: &str| {
            built
                .manifest
                .files
                .iter()
                .find(|f| f.path == name)
                .unwrap()
                .mode
        };
        assert_eq!(mode("game"), 0o755);
        assert_eq!(mode("data.bin"), 0o644);
    }

    use super::*;

    #[test]
    fn previous_pack_magic_is_rejected_with_readable_error() {
        let (frame, _) = PackFrame::from_chunk(b"previous format", None).unwrap();
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes).unwrap();
        bytes[..4].copy_from_slice(&[0x47, 0x53, 0x50, 0x46]);
        let error = PackFrame::read_from(bytes.as_slice(), MAX_FRAME_RAW_SIZE).unwrap_err();
        assert_eq!(error.to_string(), "invalid pack frame magic");
    }

    #[test]
    fn previous_manifest_magic_is_rejected_with_readable_error() {
        let crypto = CryptoContext::new([9; 32], 3, "demo").unwrap();
        let manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            ChunkingParams {
                min_size: DEFAULT_MIN_CHUNK as u32,
                avg_size: DEFAULT_AVG_CHUNK as u32,
                max_size: DEFAULT_MAX_CHUNK as u32,
            },
        );
        let mut bytes = manifest.encode_with_crypto(&crypto).unwrap();
        bytes[..4].copy_from_slice(&[0x47, 0x53, 0x50, 0x4d]);
        assert_eq!(
            Manifest::decode_with_crypto(&bytes, &crypto)
                .unwrap_err()
                .to_string(),
            "invalid encrypted manifest envelope"
        );
        assert_eq!(
            Manifest::encrypted_header(&bytes).unwrap_err().to_string(),
            "invalid encrypted manifest envelope"
        );
        assert_eq!(
            Manifest::decode(&bytes).unwrap_err().to_string(),
            "decompress manifest"
        );
    }

    #[test]
    fn frame_round_trip_checks_hash_and_size() {
        let raw = b"hello vesper".repeat(1000);
        let (frame, id) = PackFrame::from_chunk(&raw, None).unwrap();
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes).unwrap();
        let decoded = PackFrame::read_from(bytes.as_slice(), 1024 * 1024)
            .unwrap()
            .decode_payload(None)
            .unwrap();
        assert_eq!(decoded, raw);
        assert_eq!(ChunkId::from_bytes(&decoded), id);
    }

    #[test]
    fn previous_build_reuses_old_pack_without_copying_it_to_new_output() {
        let input_one = tempfile::tempdir().unwrap();
        fs::write(input_one.path().join("same.bin"), b"same content").unwrap();
        let input_two = tempfile::tempdir().unwrap();
        fs::write(input_two.path().join("same.bin"), b"same content").unwrap();
        fs::write(input_two.path().join("new.bin"), b"new content").unwrap();
        let output_one = tempfile::tempdir().unwrap();
        let output_two = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let mut first = build_from_directory(
            input_one.path(),
            output_one.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024,
        )
        .unwrap();
        first.manifest.packs[0].release_tag = Some("v1".into());
        let old_pack = first.manifest.packs[0].pack_id.clone();
        let second = build_from_directory_with_previous(
            input_two.path(),
            output_two.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            params,
            1024,
            None,
            &[PreviousBuild {
                manifest: &first.manifest,
            }],
        )
        .unwrap();
        assert!(second
            .manifest
            .packs
            .iter()
            .any(|pack| pack.pack_id == old_pack));
        assert!(!second.packs.iter().any(|path| path.ends_with(&old_pack)));
        let new_packs_dir = output_two.path().join("packs");
        assert!(second
            .packs
            .iter()
            .all(|path| path.parent() == Some(new_packs_dir.as_path())));
    }

    #[test]
    fn previous_build_reuses_manifest_locations_after_old_pack_is_removed() {
        let input_root = tempfile::tempdir().unwrap();
        let output_root = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let mut previous_manifest: Option<Box<Manifest>> = None;

        for version in 1..=4 {
            fs::write(
                input_root.path().join(format!("file-{version}.bin")),
                vec![version as u8; 4096],
            )
            .unwrap();
            let output = output_root.path().join(version.to_string());
            let result = {
                let previous = previous_manifest
                    .as_deref()
                    .map(|manifest| PreviousBuild { manifest })
                    .into_iter()
                    .collect::<Vec<_>>();
                build_from_directory_with_previous(
                    input_root.path(),
                    &output,
                    "demo".into(),
                    "windows-x64".into(),
                    version.to_string(),
                    params.clone(),
                    1024,
                    None,
                    &previous,
                )
                .unwrap()
            };
            let new_pack_paths = result.packs.clone();
            let mut manifest = result.manifest;
            for pack in &mut manifest.packs {
                if pack.release_tag.is_none() {
                    pack.release_tag = Some(format!("v{version}"));
                }
            }
            previous_manifest = Some(Box::new(manifest));
            for path in new_pack_paths {
                fs::remove_file(path).unwrap();
            }
        }

        let final_manifest = previous_manifest.unwrap();
        assert_eq!(final_manifest.version, "4");
        assert!(final_manifest
            .packs
            .iter()
            .all(|pack| pack.release_tag.is_some()));
    }

    #[test]
    fn previous_build_requires_release_tags_without_pack_files() {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("same.bin"), b"same content").unwrap();
        let output_one = tempfile::tempdir().unwrap();
        let output_two = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let first = build_from_directory(
            input.path(),
            output_one.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024,
        )
        .unwrap();
        let error = match build_from_directory_with_previous(
            input.path(),
            output_two.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            params,
            1024,
            None,
            &[PreviousBuild {
                manifest: &first.manifest,
            }],
        ) {
            Ok(_) => panic!("untagged previous pack should be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("missing release_tag"));
        assert!(error.to_string().contains("previous manifest"));
    }

    #[test]
    fn previous_build_prefers_a_tagged_duplicate_chunk_location() {
        let input = tempfile::tempdir().unwrap();
        fs::write(input.path().join("same.bin"), b"same content").unwrap();
        let output_one = tempfile::tempdir().unwrap();
        let output_two = tempfile::tempdir().unwrap();
        let output_three = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64,
            avg_size: 256,
            max_size: 1024,
        };
        let first = build_from_directory(
            input.path(),
            output_one.path(),
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024,
        )
        .unwrap();
        let mut tagged = first.manifest.clone();
        tagged.packs[0].release_tag = Some("v1".into());
        let second = build_from_directory_with_previous(
            input.path(),
            output_two.path(),
            "demo".into(),
            "windows-x64".into(),
            "2".into(),
            params.clone(),
            1024,
            None,
            &[
                PreviousBuild {
                    manifest: &first.manifest,
                },
                PreviousBuild { manifest: &tagged },
            ],
        )
        .unwrap();
        assert_eq!(second.manifest.packs[0].release_tag.as_deref(), Some("v1"));

        let _ = build_from_directory_with_previous(
            input.path(),
            output_three.path(),
            "demo".into(),
            "windows-x64".into(),
            "3".into(),
            params,
            1024,
            None,
            &[PreviousBuild {
                manifest: &second.manifest,
            }],
        )
        .unwrap();
    }

    #[test]
    fn encrypted_frame_round_trip_and_tamper_detection() {
        let raw = b"encrypted vesper".repeat(1000);
        let crypto = CryptoContext::new([7u8; 32], 42, "demo").unwrap();
        let (frame, id) = PackFrame::from_chunk(&raw, Some(&crypto)).unwrap();
        assert_eq!(frame.flags & FLAG_ENCRYPTED, FLAG_ENCRYPTED);
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes).unwrap();
        let decoded_frame = PackFrame::read_from(bytes.as_slice(), 1024 * 1024).unwrap();
        let decoded = decoded_frame.decode_payload(Some(&crypto)).unwrap();
        assert_eq!(crypto.chunk_id(&decoded), id);
        bytes[FRAME_HEADER_LEN + 1] ^= 0x01;
        let tampered = PackFrame::read_from(bytes.as_slice(), 1024 * 1024).unwrap();
        assert!(tampered.decode_payload(Some(&crypto)).is_err());
    }

    #[test]
    fn encrypted_manifest_round_trip_hides_payload() {
        let crypto = CryptoContext::new([9u8; 32], 3, "demo").unwrap();
        let manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "1.0.0".into(),
            ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
        );
        let encoded = manifest.encode_with_crypto(&crypto).unwrap();
        assert!(Manifest::decode(&encoded).is_err());
        let decoded = Manifest::decode_with_crypto(&encoded, &crypto).unwrap();
        assert_eq!(decoded.app_id, manifest.app_id);
        assert_eq!(decoded.version, manifest.version);
    }

    #[test]
    fn paths_reject_traversal_and_reserved_names() {
        assert!(validate_relative_path("bin/game.exe").is_ok());
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path("CON/save.dat").is_err());
        assert!(validate_relative_path("/absolute").is_err());
        assert!(validate_relative_path("file:stream").is_err());
        assert!(validate_relative_path("NUL").is_err());
        assert!(validate_relative_path("a/../b").is_err());
    }

    #[test]
    fn frame_parser_rejects_oversized_raw_size() {
        let mut header = [0u8; FRAME_HEADER_LEN];
        header[..4].copy_from_slice(MAGIC);
        header[4] = FRAME_VERSION;
        header[6] = Codec::Raw as u8;
        header[16..20].copy_from_slice(&(MAX_FRAME_RAW_SIZE as u32 + 1).to_le_bytes());
        assert!(PackFrame::read_from(header.as_slice(), MAX_FRAME_RAW_SIZE).is_err());
    }

    #[test]
    fn manifest_decode_has_a_decompression_bound() {
        let payload = vec![0u8; MAX_MANIFEST_DECOMPRESSED + 1];
        let compressed = zstd::stream::encode_all(payload.as_slice(), 1).unwrap();
        assert!(Manifest::decode(&compressed).is_err());
    }

    #[test]
    fn directory_build_is_reproducible() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("data")).unwrap();
        fs::write(source.path().join("data/a.txt"), vec![b'a'; 100_000]).unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let params = ChunkingParams {
            min_size: 64 * 1024,
            avg_size: 128 * 1024,
            max_size: 256 * 1024,
        };
        let a = build_from_directory(
            source.path(),
            first.path(),
            "app".into(),
            "windows-x64".into(),
            "1".into(),
            params.clone(),
            1024,
        )
        .unwrap();
        let b = build_from_directory(
            source.path(),
            second.path(),
            "app".into(),
            "windows-x64".into(),
            "1".into(),
            params,
            1024,
        )
        .unwrap();
        assert_eq!(a.manifest.encode().unwrap(), b.manifest.encode().unwrap());
        assert_eq!(
            fs::read(
                first
                    .path()
                    .join("packs")
                    .join(&a.manifest.packs[0].pack_id)
            )
            .unwrap(),
            fs::read(
                second
                    .path()
                    .join("packs")
                    .join(&b.manifest.packs[0].pack_id)
            )
            .unwrap()
        );
    }
}
