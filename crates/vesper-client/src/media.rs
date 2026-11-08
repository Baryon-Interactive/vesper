use crate::http::client;
use anyhow::Result;

/// Check an HTTP media response against its expected BLAKE3 hash.
pub fn verify_media(url: &str, expected_blake3: &str) -> Result<bool> {
    if !matches!(url.split(':').next(), Some("http" | "https")) || expected_blake3.len() != 64 {
        return Ok(false);
    };
    let bytes = client()?.get(url).send()?.error_for_status()?.bytes()?;
    Ok(hex::encode(blake3::hash(&bytes).as_bytes()) == expected_blake3.to_ascii_lowercase())
}
