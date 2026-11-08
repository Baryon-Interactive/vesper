use anyhow::{Context, Result};
use std::{env, fs, path::PathBuf};
use vesper_format::hash_bytes;
use vesper_meta::{
    read_json, sign, write_json, AppChannel, AppChannelManifest, AppTargetsSigned, KeyPair, Signed,
};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let first = args.next().context("targets path or --media-hash")?;
    if first == "--media-hash" {
        let bytes = fs::read(args.next().context("media path")?)?;
        println!("{}", hex::encode(hash_bytes(&bytes)));
        return Ok(());
    }
    let targets_path = PathBuf::from(first);
    let manifest_path = PathBuf::from(args.next().context("manifest path")?);
    let release_tag = args.next().context("release tag")?;
    let app_id = args.next().context("app id")?;
    let key_hex = env::var("VESPER_DEV_KEY").context("VESPER_DEV_KEY")?;
    let mut document: Signed<AppTargetsSigned> = read_json(&targets_path)?;
    let manifest = fs::read(&manifest_path)?;
    let channel = document
        .signed
        .channels
        .get("stable")
        .cloned()
        .context("stable channel")?;
    document.signed.expires = vesper_meta::expires_in(90);
    document.signed.channels.insert(
        "stable".into(),
        AppChannel {
            version: channel.version,
            platform: "windows-x64".into(),
            manifest: AppChannelManifest {
                release_tag,
                asset_name: "manifest.cbor.zst".into(),
                size: manifest.len() as u64,
                blake3: hex::encode(hash_bytes(&manifest)),
            },
            encrypted: true,
            key_id: 1,
        },
    );
    document.signed.metadata =
        vesper_meta::store_metadata(targets_path.parent().context("targets directory")?)?;
    let key = KeyPair::from_secret_hex(&key_hex)?;
    write_json(&targets_path, &sign(document.signed, &[&key])?)?;
    println!("signed encrypted targets for {app_id}");
    Ok(())
}
