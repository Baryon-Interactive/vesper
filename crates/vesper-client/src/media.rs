use crate::http::client;
use anyhow::{ensure, Result};
use std::io::Read;

/// Return the exact verified media bytes for display through a frontend blob.
pub fn verify_media(url: &str, expected_blake3: &str) -> Result<Vec<u8>> {
    if !matches!(url.split(':').next(), Some("http" | "https")) || expected_blake3.len() != 64 {
        anyhow::bail!("invalid media URL or hash");
    };
    let response = client()?.get(url).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16 * 1024 * 1024, "media exceeds 16 MiB");
    ensure!(
        hex::encode(blake3::hash(&bytes).as_bytes()) == expected_blake3.to_ascii_lowercase(),
        "media BLAKE3 mismatch"
    );
    Ok(bytes)
}
