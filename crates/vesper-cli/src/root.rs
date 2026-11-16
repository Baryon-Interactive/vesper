//! Offline catalogue root renewal and rotation.
//!
//! A new root is drafted from the current `root.json`, signed by a threshold of
//! the current root keys and a threshold of its own root keys (which may be the
//! same keys), then published as `<N>.root.json` next to `root.json`. Clients
//! follow the versioned files from the root they already trust.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use vesper_meta::{
    expires_in, read_json, versioned_root_name, write_json, KeyPair, RootSigned, RoutesSigned,
    Signed,
};

#[derive(clap::Args)]
pub struct DraftArgs {
    #[arg(long)]
    pub index: PathBuf,
    /// Draft output; defaults to <index>/root.draft.json.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Lifetime of the new root in days.
    #[arg(long, default_value_t = 365)]
    pub expires_days: u64,
    /// Add an offline root public key (hex). Repeatable.
    #[arg(long = "add-root-key")]
    pub add_root_keys: Vec<String>,
    /// Remove a root key by key id or public key (hex). Repeatable.
    #[arg(long = "remove-root-key")]
    pub remove_root_keys: Vec<String>,
    /// New root signature threshold; defaults to the current threshold.
    #[arg(long)]
    pub threshold: Option<u32>,
    /// Replace the snapshot/timestamp signing key with this public key (hex).
    #[arg(long)]
    pub online_pubkey: Option<String>,
}

pub fn default_draft(index: &Path) -> PathBuf {
    index.join("root.draft.json")
}

/// Generate an offline root key, refusing to overwrite an existing file.
pub fn keygen(out: &Path) -> Result<()> {
    let key = KeyPair::generate();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .with_context(|| format!("create {}", out.display()))?;
    std::io::Write::write_all(&mut file, hex::encode(key.signing.to_bytes()).as_bytes())?;
    println!("{}", key.public_hex());
    eprintln!("key id {}; keep {} offline", key.keyid, out.display());
    Ok(())
}

fn public_key(hex_value: &str) -> Result<(String, String)> {
    let bytes = hex::decode(hex_value.trim()).context("public key must be hex")?;
    let id = vesper_meta::public_key_id(&bytes)?;
    Ok((id, hex_value.trim().to_ascii_lowercase()))
}

/// Write an unsigned draft of the next root version.
pub fn draft(args: &DraftArgs) -> Result<()> {
    let current: Signed<RootSigned> = read_json(&args.index.join("root.json"))?;
    vesper_meta::verify_root_self(&current).context("current root.json")?;
    let mut next = current.signed.clone();
    next.version = next
        .version
        .checked_add(1)
        .context("root version overflow")?;
    next.expires = expires_in(args.expires_days);
    let mut role = next.roles.get("root").context("root role missing")?.clone();
    for value in &args.remove_root_keys {
        let value_lower = value.trim().to_ascii_lowercase();
        let id = if role.keyids.contains(&value_lower) {
            value_lower
        } else {
            public_key(value)?.0
        };
        anyhow::ensure!(role.keyids.contains(&id), "{value} is not a root key");
        role.keyids.retain(|keyid| keyid != &id);
    }
    for value in &args.add_root_keys {
        let (id, public) = public_key(value)?;
        anyhow::ensure!(!role.keyids.contains(&id), "{value} is already a root key");
        next.keys.insert(id.clone(), public);
        role.keyids.push(id);
    }
    if let Some(threshold) = args.threshold {
        role.threshold = threshold;
    }
    anyhow::ensure!(
        role.threshold >= 1 && role.threshold as usize <= role.keyids.len(),
        "root threshold {} is invalid for {} root keys",
        role.threshold,
        role.keyids.len()
    );
    next.roles.insert("root".into(), role);
    if let Some(value) = &args.online_pubkey {
        let (id, public) = public_key(value)?;
        next.keys.insert(id.clone(), public);
        for name in ["snapshot", "timestamp"] {
            next.roles.insert(
                name.into(),
                vesper_meta::RoleSpec {
                    keyids: vec![id.clone()],
                    threshold: 1,
                },
            );
        }
    }
    // Drop keys that no role references any more.
    let used: std::collections::BTreeSet<_> = next
        .roles
        .values()
        .flat_map(|role| role.keyids.iter())
        .collect();
    next.keys.retain(|id, _| used.contains(id));
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| default_draft(&args.index));
    write_json(
        &out,
        &Signed {
            signed: next.clone(),
            signatures: Vec::new(),
        },
    )?;
    let old_role = &current.signed.roles["root"];
    let new_role = &next.roles["root"];
    println!(
        "drafted root version {} in {}; sign with {} of the version {} root keys and {} of the new root keys, then run `vesper index root-publish`",
        next.version,
        out.display(),
        old_role.threshold,
        current.signed.version,
        new_role.threshold
    );
    Ok(())
}

/// Add one offline key's signature to a root draft.
pub fn sign(draft: &Path, key_file: &Path) -> Result<()> {
    let doc: Signed<RootSigned> = read_json(draft)?;
    let key = KeyPair::from_secret_hex(&fs::read_to_string(key_file)?)?;
    let mut signed = vesper_meta::sign(doc.signed, &[&key])?;
    signed.signatures.extend(
        doc.signatures
            .into_iter()
            .filter(|entry| entry.keyid != key.keyid),
    );
    write_json(draft, &signed)?;
    println!(
        "signed root version {} with key {} ({} signatures)",
        signed.signed.version,
        key.keyid,
        signed.signatures.len()
    );
    Ok(())
}

/// Verify a signed draft against the current root and publish it as
/// `<N>.root.json` and `root.json`. The current root is also kept as its
/// versioned file so every client can chain from the root it already holds.
pub fn publish(index: &Path, draft: &Path) -> Result<()> {
    let current: Signed<RootSigned> = read_json(&index.join("root.json"))?;
    vesper_meta::verify_root_self(&current).context("current root.json")?;
    let next: Signed<RootSigned> = read_json(draft)?;
    vesper_meta::verify_root_successor(&current, &next)?;
    anyhow::ensure!(
        next.signed.expires > vesper_meta::now_unix(),
        "new root has already expired"
    );
    for root in [&current, &next] {
        let path = index.join(versioned_root_name(root.signed.version));
        if path.exists() {
            let existing: Signed<RootSigned> = read_json(&path)?;
            anyhow::ensure!(
                same(&existing, root)?,
                "{} already exists with different content",
                path.display()
            );
        } else {
            write_json(&path, root)?;
        }
    }
    write_json(&index.join("root.json"), &next)?;
    fs::remove_file(draft).with_context(|| format!("remove {}", draft.display()))?;
    println!(
        "published root version {} as {} and root.json",
        next.signed.version,
        versioned_root_name(next.signed.version)
    );
    let routes: Signed<RoutesSigned> = read_json(&index.join("routes.json"))?;
    if let Err(error) = vesper_meta::verify_routes(&routes, &next) {
        eprintln!(
            "warning: routes.json does not verify under the new root ({error:#}); \
             re-sign it with the new root keys using `vesper index route-sign`"
        );
    }
    Ok(())
}

fn same(a: &Signed<RootSigned>, b: &Signed<RootSigned>) -> Result<bool> {
    Ok(serde_json::to_value(a)? == serde_json::to_value(b)?)
}

/// Versioned root files present in an index, keyed by the version in the name.
fn versioned_roots(index: &Path) -> Result<BTreeMap<u64, Signed<RootSigned>>> {
    let mut roots = BTreeMap::new();
    for entry in fs::read_dir(index)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let Some(version) = name
            .strip_suffix(".root.json")
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let version: u64 = version.parse().context("root file version")?;
        anyhow::ensure!(
            name == versioned_root_name(version),
            "non-canonical root file name {name}"
        );
        roots.insert(version, read_json(&index.join(&name))?);
    }
    Ok(roots)
}

/// Verify `root.json` and the versioned root chain of an index. With a
/// previous index, the new chain must extend the previous `root.json`.
pub fn verify_index_roots(index: &Path, previous: Option<&Path>) -> Result<Signed<RootSigned>> {
    let current: Signed<RootSigned> = read_json(&index.join("root.json"))?;
    vesper_meta::verify_root_self(&current).context("root.json")?;
    let roots = versioned_roots(index)?;
    if let (Some((&first, first_root)), Some((&last, last_root))) =
        (roots.first_key_value(), roots.last_key_value())
    {
        vesper_meta::verify_root_self(first_root).with_context(|| versioned_root_name(first))?;
        anyhow::ensure!(
            first_root.signed.version == first,
            "{} holds root version {}",
            versioned_root_name(first),
            first_root.signed.version
        );
        for version in first + 1..=last {
            let next = roots.get(&version).with_context(|| {
                format!(
                    "root chain gap: {} is missing",
                    versioned_root_name(version)
                )
            })?;
            vesper_meta::verify_root_successor(&roots[&(version - 1)], next)
                .with_context(|| versioned_root_name(version))?;
        }
        anyhow::ensure!(
            same(&current, last_root)?,
            "root.json must equal the newest versioned root {}",
            versioned_root_name(last)
        );
    } else {
        anyhow::ensure!(
            current.signed.version == 1,
            "root.json version {} needs its versioned root files",
            current.signed.version
        );
    }
    if let Some(base) = previous {
        let old: Signed<RootSigned> = read_json(&base.join("root.json"))?;
        if !same(&old, &current)? {
            anyhow::ensure!(
                current.signed.version > old.signed.version,
                "root.json version rollback: {} < {}",
                current.signed.version,
                old.signed.version
            );
            let chained = roots.get(&old.signed.version);
            anyhow::ensure!(
                chained
                    .map(|root| same(root, &old))
                    .transpose()?
                    .unwrap_or(false),
                "the new root chain does not include the previous root.json version {}",
                old.signed.version
            );
        }
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_key(dir: &Path, name: &str) -> (PathBuf, KeyPair) {
        let path = dir.join(name);
        let key = KeyPair::generate();
        fs::write(&path, hex::encode(key.signing.to_bytes())).unwrap();
        (path, key)
    }

    fn draft_args(index: &Path) -> DraftArgs {
        DraftArgs {
            index: index.to_path_buf(),
            out: None,
            expires_days: 365,
            add_root_keys: Vec::new(),
            remove_root_keys: Vec::new(),
            threshold: None,
            online_pubkey: None,
        }
    }

    #[test]
    fn renew_and_rotate_publish_a_verifiable_chain() {
        let temp = tempfile::tempdir().unwrap();
        let index = temp.path().join("index");
        let keys = temp.path().join("keys");
        let online = KeyPair::generate();
        crate::index_init(&index, 3, 2, &keys, Some(&online.public_hex())).unwrap();
        let base = temp.path().join("base");
        fs::create_dir_all(&base).unwrap();
        fs::copy(index.join("root.json"), base.join("root.json")).unwrap();
        let root_key = |n: usize| keys.join(format!("root-{n}.key"));

        // Renew: same keys, new expiry.
        draft(&draft_args(&index)).unwrap();
        let pending = default_draft(&index);
        sign(&pending, &root_key(0)).unwrap();
        assert!(
            publish(&index, &pending).is_err(),
            "one signature is below threshold"
        );
        sign(&pending, &root_key(0)).unwrap(); // re-signing does not count twice
        assert!(publish(&index, &pending).is_err());
        sign(&pending, &root_key(2)).unwrap();
        publish(&index, &pending).unwrap();
        assert!(index.join("1.root.json").exists());
        assert!(!pending.exists());
        let renewed = verify_index_roots(&index, Some(&base)).unwrap();
        assert_eq!(renewed.signed.version, 2);

        // Rotate: replace root key 1 with a new key and change the online key.
        let (new_key_file, new_key) = write_key(temp.path(), "new.key");
        let old_id = KeyPair::from_secret_hex(&fs::read_to_string(root_key(1)).unwrap())
            .unwrap()
            .keyid;
        let online2 = KeyPair::generate();
        let mut args = draft_args(&index);
        args.add_root_keys = vec![new_key.public_hex()];
        args.remove_root_keys = vec![old_id.clone()];
        args.online_pubkey = Some(online2.public_hex());
        draft(&args).unwrap();
        // A threshold of the new keys alone is not enough: version 2 keys must agree.
        sign(&pending, &new_key_file).unwrap();
        sign(&pending, &root_key(2)).unwrap();
        assert!(publish(&index, &pending).is_err());
        sign(&pending, &root_key(0)).unwrap();
        publish(&index, &pending).unwrap();
        let rotated = verify_index_roots(&index, Some(&base)).unwrap();
        assert_eq!(rotated.signed.version, 3);
        assert!(!rotated.signed.keys.contains_key(&old_id));
        assert!(!rotated.signed.keys.contains_key(&online.keyid));
        assert_eq!(
            rotated.signed.roles["timestamp"].keyids,
            vec![online2.keyid]
        );
        assert!(rotated.signed.roles["root"].keyids.contains(&new_key.keyid));
    }

    #[test]
    fn index_verification_rejects_forged_and_rolled_back_roots() {
        let temp = tempfile::tempdir().unwrap();
        let index = temp.path().join("index");
        let keys = temp.path().join("keys");
        crate::index_init(&index, 2, 2, &keys, Some(&KeyPair::generate().public_hex())).unwrap();
        let v1: Signed<RootSigned> = read_json(&index.join("root.json")).unwrap();
        draft(&draft_args(&index)).unwrap();
        let pending = default_draft(&index);
        for n in 0..2 {
            sign(&pending, &keys.join(format!("root-{n}.key"))).unwrap();
        }
        publish(&index, &pending).unwrap();
        let v2: Signed<RootSigned> = read_json(&index.join("root.json")).unwrap();
        verify_index_roots(&index, None).unwrap();

        // A forged 3.root.json signed by an unknown key breaks the chain.
        let attacker = KeyPair::generate();
        let mut forged = v2.signed.clone();
        forged.version = 3;
        forged
            .keys
            .insert(attacker.keyid.clone(), attacker.public_hex());
        forged.roles.get_mut("root").unwrap().keyids = vec![attacker.keyid.clone()];
        forged.roles.get_mut("root").unwrap().threshold = 1;
        let forged = vesper_meta::sign(forged, &[&attacker]).unwrap();
        write_json(&index.join("3.root.json"), &forged).unwrap();
        write_json(&index.join("root.json"), &forged).unwrap();
        assert!(verify_index_roots(&index, None).is_err());
        fs::remove_file(index.join("3.root.json")).unwrap();

        // root.json rolled back below the newest versioned root.
        write_json(&index.join("root.json"), &v1).unwrap();
        assert!(verify_index_roots(&index, None).is_err());

        // A new index must extend, not replace, the previous root.json.
        write_json(&index.join("root.json"), &v2).unwrap();
        let base = temp.path().join("base");
        fs::create_dir_all(&base).unwrap();
        write_json(&base.join("root.json"), &v2).unwrap();
        fs::remove_file(index.join("2.root.json")).unwrap();
        write_json(&index.join("root.json"), &v1).unwrap();
        assert!(verify_index_roots(&index, Some(&base)).is_err());
    }
}
