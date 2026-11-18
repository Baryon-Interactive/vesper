//! `vesper store`: write and check the store listing in a metadata directory.
use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use vesper_meta::store;

#[derive(Subcommand)]
pub enum StoreCommand {
    /// Start a listing: store.json, description.md and changelog.md.
    Init {
        #[arg(long, default_value = "metadata")]
        dir: PathBuf,
        #[arg(long)]
        title: String,
        #[arg(long)]
        developer: String,
        #[arg(long)]
        homepage: String,
        /// Platforms the game ships on, for example web-wasm32 (repeat the flag for more).
        #[arg(long = "platform", required = true)]
        platforms: Vec<String>,
    },
    /// Check store.json against the store rules and the documents next to it.
    Check {
        #[arg(long, default_value = "metadata")]
        dir: PathBuf,
    },
    /// Hash an image and record it in store.json with its size and public URL.
    AddMedia {
        /// The local copy of the image (PNG, JPEG, WebP or GIF).
        file: PathBuf,
        #[arg(long = "as", value_enum)]
        role: MediaRole,
        /// Where players' browsers load the image from (https://).
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "metadata")]
        dir: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum MediaRole {
    /// Wide header image (the required one).
    Cover,
    /// Portrait image for shelves, about 600x900.
    Capsule,
    /// Transparent title logo.
    Logo,
    /// Page background.
    Background,
    /// Appended to the screenshot list.
    Screenshot,
}

pub fn run(command: StoreCommand) -> Result<()> {
    match command {
        StoreCommand::Init {
            dir,
            title,
            developer,
            homepage,
            platforms,
        } => init(&dir, &title, &developer, &homepage, &platforms),
        StoreCommand::Check { dir } => {
            let problems = check(&dir)?;
            if problems.is_empty() {
                println!("{} is a valid store listing", dir.display());
                return Ok(());
            }
            for problem in &problems {
                eprintln!("{problem}");
            }
            anyhow::bail!("{} problem(s) in {}", problems.len(), dir.display())
        }
        StoreCommand::AddMedia {
            file,
            role,
            url,
            dir,
        } => add_media(&dir, &file, role, &url),
    }
}

fn init(
    dir: &Path,
    title: &str,
    developer: &str,
    homepage: &str,
    platforms: &[String],
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let listing = json!({
        "title": {"en": title},
        "summary": {"en": format!("{title} in one sentence.")},
        "tags": ["indie"],
        "screenshots": [],
        "requirements": {},
        "paid": false,
        "price_display": "Free",
        "developer": developer,
        "homepage": homepage,
        "platforms": platforms,
        "default_language": "en",
        "release": {"status": "released"},
        "languages": {"en": {"interface": true, "audio": false, "subtitles": false}},
    });
    let files = [
        ("store.json", serde_json::to_string_pretty(&listing)? + "\n"),
        (
            "description.md",
            format!("# {title}\n\nTell players what the game is about.\n"),
        ),
        ("changelog.md", "## 1.0.0\n\nFirst release.\n".to_owned()),
    ];
    for (name, text) in files {
        let path = dir.join(name);
        if path.exists() {
            println!("kept {}", path.display());
        } else {
            fs::write(&path, text)?;
            println!("wrote {}", path.display());
        }
    }
    println!(
        "next: vesper store add-media <image> --as cover --url <https URL>, then vesper store check --dir {}",
        dir.display()
    );
    Ok(())
}

/// Every problem with the listing in `dir`; empty when it can be published.
pub fn check(dir: &Path) -> Result<Vec<String>> {
    let path = dir.join("store.json");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => return Ok(vec![format!("store.json: invalid JSON ({error})")]),
    };
    let mut problems: Vec<String> = store::validate(&value)
        .into_iter()
        .map(|p| format!("store.json: {p}"))
        .collect();
    if let Err(error) = vesper_meta::store_metadata(dir) {
        problems.push(format!("{error:#}"));
    }
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let looks_like_store_file =
            name.starts_with("description.") || name.starts_with("changelog.");
        if looks_like_store_file && !store::is_store_file_name(&name) {
            problems.push(format!(
                "{name} is not signed: name documents description.<language>.md or changelog.<language>.md"
            ));
        }
    }
    Ok(problems)
}

fn add_media(dir: &Path, file: &Path, role: MediaRole, url: &str) -> Result<()> {
    let bytes = fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let (width, height) = image_size(&bytes)
        .with_context(|| format!("{} is not a PNG, JPEG, WebP or GIF image", file.display()))?;
    anyhow::ensure!(
        url.starts_with("https://") && !url.contains(char::is_whitespace),
        "--url must be an https:// URL"
    );
    let media = json!({
        "url": url,
        "blake3": blake3::hash(&bytes).to_hex().to_string(),
        "width": width,
        "height": height,
    });
    let path = dir.join("store.json");
    let mut listing: Value = serde_json::from_str(
        &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .context("store.json is not valid JSON")?;
    let object = listing
        .as_object_mut()
        .context("store.json must be an object")?;
    let key = match role {
        MediaRole::Cover => "cover",
        MediaRole::Capsule => "capsule",
        MediaRole::Logo => "logo",
        MediaRole::Background => "background",
        MediaRole::Screenshot => "screenshots",
    };
    if let MediaRole::Screenshot = role {
        let list = object
            .entry("screenshots")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("screenshots must be a list")?;
        list.retain(|item| item.get("url").and_then(Value::as_str) != Some(url));
        list.push(media);
    } else {
        object.insert(key.into(), media);
    }
    fs::write(&path, serde_json::to_string_pretty(&listing)? + "\n")?;
    println!("{key}: {url} ({width}x{height})");
    Ok(())
}

/// Width and height from the header of a PNG, JPEG, WebP or GIF image.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32);
    let le16 = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32);
    let le24 = |at: usize| {
        let b = bytes.get(at..at + 3)?;
        Some(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
    };
    let size = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR") {
        let be32 = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        (be32(16)?, be32(20)?)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        (le16(6)?, le16(8)?)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        match bytes.get(12..16)? {
            b"VP8 " => (le16(26)? & 0x3fff, le16(28)? & 0x3fff),
            b"VP8L" => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" => (le24(24)? + 1, le24(27)? + 1),
            _ => return None,
        }
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let mut at = 2;
        loop {
            while *bytes.get(at)? != 0xff {
                at += 1;
            }
            while *bytes.get(at)? == 0xff {
                at += 1;
            }
            let marker = *bytes.get(at)?;
            at += 1;
            if matches!(marker, 0xd0..=0xd9 | 0x01) {
                continue;
            }
            let length = be16(at)? as usize;
            // Start-of-frame markers, except DHT (c4), JPG (c8) and DAC (cc).
            if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                break (be16(at + 5)?, be16(at + 3)?);
            }
            at += length;
        }
    } else {
        return None;
    };
    (size.0 > 0 && size.1 > 0).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0]);
        bytes
    }

    #[test]
    fn reads_image_sizes_from_headers() {
        assert_eq!(image_size(&png(1920, 1080)), Some((1920, 1080)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0x58, 0x02, 0x84, 0x03]);
        assert_eq!(image_size(&gif), Some((600, 900)));
        // JPEG: SOI, an APP0 segment, then SOF0 with height 480 and width 640.
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01,
            0xe0, 0x02, 0x80,
        ];
        assert_eq!(image_size(&jpeg), Some((640, 480)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        webp.extend([0x7f, 0x07, 0x00, 0x37, 0x04, 0x00]);
        assert_eq!(image_size(&webp), Some((1920, 1080)));
        assert_eq!(image_size(b"not an image"), None);
        assert_eq!(image_size(&png(0, 10)), None);
    }

    #[test]
    fn init_add_media_and_check_build_a_valid_listing() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("metadata");
        init(
            &meta,
            "Example",
            "Studio",
            "https://example.com",
            &["web-wasm32".into()],
        )
        .unwrap();
        let problems = check(&meta).unwrap();
        assert_eq!(problems, vec!["store.json: missing cover".to_owned()]);

        let image = dir.path().join("cover.png");
        fs::write(&image, png(1280, 720)).unwrap();
        add_media(
            &meta,
            &image,
            MediaRole::Cover,
            "https://cdn.example.com/cover.png",
        )
        .unwrap();
        add_media(
            &meta,
            &image,
            MediaRole::Screenshot,
            "https://cdn.example.com/1.png",
        )
        .unwrap();
        add_media(
            &meta,
            &image,
            MediaRole::Screenshot,
            "https://cdn.example.com/1.png",
        )
        .unwrap();
        assert_eq!(check(&meta).unwrap(), Vec::<String>::new());

        let listing: Value =
            serde_json::from_slice(&fs::read(meta.join("store.json")).unwrap()).unwrap();
        assert_eq!(listing["cover"]["width"], 1280);
        assert_eq!(
            listing["cover"]["blake3"],
            blake3::hash(&png(1280, 720)).to_hex().as_str()
        );
        assert_eq!(listing["screenshots"].as_array().unwrap().len(), 1);
        assert!(add_media(
            &meta,
            &image,
            MediaRole::Logo,
            "http://cdn.example.com/logo.png"
        )
        .is_err());
    }

    #[test]
    fn check_reports_rule_breaks_and_misnamed_documents() {
        let dir = tempfile::tempdir().unwrap();
        init(
            dir.path(),
            "Example",
            "Studio",
            "https://example.com",
            &["web-wasm32".into()],
        )
        .unwrap();
        let path = dir.path().join("store.json");
        let mut listing: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        listing["max_players"] = json!(0);
        fs::write(&path, listing.to_string()).unwrap();
        fs::write(dir.path().join("changelog.english.md"), "notes").unwrap();
        let problems = check(dir.path()).unwrap().join("\n");
        assert!(problems.contains("max_players"), "{problems}");
        assert!(
            problems.contains("changelog.english.md is not signed"),
            "{problems}"
        );
    }
}
