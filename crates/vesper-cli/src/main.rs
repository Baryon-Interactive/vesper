use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;
use vesper_fetch::{MultiSourceFetcher, Source, SourceSpec};
use vesper_format::{
    gc_report, ChunkingParams, CryptoContext, Manifest, DEFAULT_AVG_CHUNK, DEFAULT_MAX_CHUNK,
    DEFAULT_MIN_CHUNK,
};
use vesper_install::Installer;
use vesper_meta::{
    build_snapshot, build_timestamp, expires_in, read_json, sign, validate_version_and_expiry,
    verify_metadata_description, verify_role, write_json, AppChannel, AppChannelManifest, AppRoute,
    AppSource, AppTargetsSigned, KeyPair, RoleSpec, RootSigned, RoutesSigned, Signed,
    SnapshotSigned, TargetsSigned, TimestampSigned,
};
use vesper_publish::r2::{R2Config, R2Publisher};
use vesper_publish::{token_from_env_or_gh, GitHubPublisher};

mod root;

#[derive(clap::Args, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct R2Options {
    #[arg(long = "r2-bucket")]
    bucket: Option<String>,
    #[arg(long = "r2-account-id")]
    account_id: Option<String>,
    #[arg(long = "r2-endpoint")]
    endpoint: Option<String>,
    #[arg(long = "r2-prefix")]
    prefix: Option<String>,
    #[arg(long = "r2-public-base")]
    public_base: Option<String>,
}

impl R2Options {
    fn resolve(self) -> Result<Option<R2Config>> {
        #[derive(serde::Deserialize, Default)]
        struct Config {
            r2: Option<R2Options>,
        }
        let file: Config = if std::path::Path::new("vesper.toml").exists() {
            toml::from_str(&fs::read_to_string("vesper.toml")?).map_err(|_| {
                anyhow::anyhow!(
                    "invalid vesper.toml configuration (R2 accepts only non-secret settings)"
                )
            })?
        } else {
            Config::default()
        };
        let old = file.r2.unwrap_or_default();
        let options = Self {
            bucket: self.bucket.or(old.bucket),
            account_id: self.account_id.or(old.account_id),
            endpoint: self.endpoint.or(old.endpoint),
            prefix: self.prefix.or(old.prefix),
            public_base: self.public_base.or(old.public_base),
        };
        if options.bucket.is_none()
            && options.account_id.is_none()
            && options.endpoint.is_none()
            && options.prefix.is_none()
            && options.public_base.is_none()
        {
            return Ok(None);
        }
        let config = R2Config {
            bucket: options.bucket.context("R2 bucket is required")?,
            account_id: options.account_id.unwrap_or_default(),
            endpoint: options.endpoint,
            prefix: options.prefix.unwrap_or_default(),
            public_base: options.public_base.context("R2 public_base is required")?,
        };
        config.validate()?;
        vesper_meta::validate_url_template(&config.url_template())?;
        Ok(Some(config))
    }
}

#[derive(Parser)]
#[command(
    name = "vesper",
    version = env!("VESPER_CLI_VERSION"),
    about = "Vesper Vesper/1 verified build distribution CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a developer project configuration and key registration.
    Init {
        #[arg(long, default_value = "vesper.toml")]
        output: PathBuf,
        #[arg(long)]
        app_id: Option<String>,
        #[arg(long)]
        index: Option<PathBuf>,
    },
    Pack {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        app_id: String,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long, default_value_t = DEFAULT_MIN_CHUNK)]
        min_chunk: usize,
        #[arg(long, default_value_t = DEFAULT_AVG_CHUNK)]
        avg_chunk: usize,
        #[arg(long, default_value_t = DEFAULT_MAX_CHUNK)]
        max_chunk: usize,
        #[arg(long, default_value_t = 512 * 1024 * 1024)]
        max_pack_size: u64,
        /// Previous manifest(s) used for content-addressed pack reuse.
        #[arg(long = "previous")]
        previous: Vec<PathBuf>,
        #[arg(long)]
        release_tag: Option<String>,
        /// Path globs preserved during update/repair, e.g. Saved/** or *.ini.
        #[arg(long = "user-file")]
        user_files: Vec<String>,
        #[arg(long)]
        depot_key_hex: Option<String>,
        #[arg(long, default_value_t = 1)]
        key_id: u64,
    },
    Check {
        #[arg(value_name = "BUILD")]
        build: Option<PathBuf>,
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long, default_value_t = 4 * 1024 * 1024 * 1024)]
        max_file_size: u64,
        #[arg(long)]
        json: bool,
    },
    DiffCheck {
        #[arg(value_name = "BUILD")]
        build: Option<PathBuf>,
        #[arg(long)]
        current: Option<PathBuf>,
        #[arg(long)]
        against: PathBuf,
        #[arg(long)]
        depot_key_hex: Option<String>,
        #[arg(long)]
        key_id: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Publish {
        #[command(flatten)]
        r2: R2Options,
        /// Local developer metadata directory (store, description and version history).
        #[arg(long, conflicts_with = "index")]
        metadata_dir: Option<PathBuf>,
        /// Mutable metadata release, separate from immutable game build releases.
        #[arg(long, default_value = "vesper-metadata")]
        metadata_tag: String,
        /// Release tag; defaults to the manifest version. Required when pack tags exclude that version.
        #[arg(long)]
        tag: Option<String>,
        #[arg(value_name = "BUILD")]
        build: PathBuf,
        #[arg(long, default_value = "stable")]
        channel: String,
        #[arg(long)]
        index: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, env = "GITHUB_TOKEN")]
        token: Option<String>,
        #[arg(long)]
        api_base: Option<String>,
        #[arg(long)]
        upload_base: Option<String>,
    },
    Promote {
        from: String,
        to: String,
        #[arg(long)]
        index: PathBuf,
    },
    Rollback {
        channel: String,
        #[arg(long)]
        index: PathBuf,
    },
    InstallRollback {
        #[arg(long)]
        target: PathBuf,
    },
    Gc {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    Sign {
        #[arg(long)]
        targets: PathBuf,
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    Launch {
        #[arg(long)]
        target: PathBuf,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
    },
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    Inspect {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        depot_key_hex: Option<String>,
        #[arg(long)]
        key_id: Option<u64>,
    },
    Verify {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        depot_key_hex: Option<String>,
        #[arg(long)]
        key_id: Option<u64>,
    },
    Install {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long = "source", num_args = 1..)]
        sources: Vec<String>,
        /// Default release tag used when a pack entry has no release_tag.
        #[arg(long)]
        release: Option<String>,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        depot_key_hex: Option<String>,
        #[arg(long)]
        key_id: Option<u64>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    Uninstall {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long)]
        delete_user_files: bool,
    },
    MetaInit {
        #[arg(long)]
        output: PathBuf,
    },
    MetaVerify {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        targets: PathBuf,
        #[arg(long)]
        previous_targets_version: Option<u64>,
        #[arg(long)]
        snapshot: Option<PathBuf>,
        #[arg(long)]
        timestamp: Option<PathBuf>,
    },
    MetaRefresh {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        key_file: Option<PathBuf>,
        #[arg(long)]
        snapshot_key_file: Option<PathBuf>,
        #[arg(long)]
        timestamp_key_file: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        snapshot_version: u64,
        #[arg(long, default_value_t = 1)]
        timestamp_version: u64,
    },
    MetaAddTarget {
        #[arg(long)]
        targets: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        app_id: String,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        version: String,
        #[arg(long, default_value = "stable")]
        channel: String,
    },
    PublishGithub {
        #[arg(long)]
        api_base: Option<String>,
        #[arg(long)]
        upload_base: Option<String>,
        #[arg(long)]
        index: Option<PathBuf>,
        #[arg(long, default_value = "stable")]
        channel: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        tag: String,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        packs_dir: PathBuf,
        #[arg(long, env = "GITHUB_TOKEN")]
        token: Option<String>,
        #[arg(long)]
        depot_key_hex: Option<String>,
        #[arg(long)]
        key_id: Option<u64>,
    },
}

#[derive(Subcommand)]
enum IndexCommand {
    /// Print the public key derived from VESPER_ONLINE_KEY.
    OnlinePubkey,
    Verify {
        #[arg(long)]
        index: PathBuf,
        #[arg(long = "app")]
        apps: Vec<String>,
        #[arg(long)]
        previous: Option<PathBuf>,
        /// Verify developer targets before online signing in an untrusted PR.
        #[arg(long)]
        targets_only: bool,
    },
    /// List only apps whose targets changed or whose snapshot expires within two days.
    PendingSnapshots {
        #[arg(long)]
        index: PathBuf,
    },
    Snapshot {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        app: String,
        #[arg(long)]
        previous: Option<PathBuf>,
        /// Permit first-time online signing only when no prior version is available.
        #[arg(long)]
        initialize: bool,
    },
    Timestamp {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        previous: Option<PathBuf>,
        #[arg(long)]
        initialize: bool,
    },
    RouteAdd {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        app: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long)]
        metadata_url: Option<String>,
    },
    /// Fetch and verify developer-hosted metadata, retaining each app on failure.
    Aggregate {
        #[arg(long)]
        index: PathBuf,
    },
    RouteSign {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        root_key_file: PathBuf,
    },
    /// Generate an offline root key file and print its public key.
    RootKeygen {
        #[arg(long)]
        out: PathBuf,
    },
    /// Draft the next root version (renewal or key rotation) for offline signing.
    RootDraft(root::DraftArgs),
    /// Add one offline root key's signature to a root draft.
    RootSign {
        #[arg(long)]
        draft: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Verify a signed root draft and publish it as <N>.root.json and root.json.
    RootPublish {
        #[arg(long)]
        index: PathBuf,
        /// Defaults to <index>/root.draft.json.
        #[arg(long)]
        draft: Option<PathBuf>,
    },
    Init {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        root_keys: usize,
        #[arg(long)]
        threshold: u32,
        #[arg(long)]
        out_keys: PathBuf,
        /// Snapshot and timestamp signing public key; otherwise derived from VESPER_ONLINE_KEY.
        #[arg(long)]
        online_pubkey: Option<String>,
    },
}

fn main() -> Result<()> {
    // The derived argument parser for this many subcommands needs more than the
    // 1 MiB Windows main-thread stack in unoptimised builds.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| run_command(Cli::parse().command))?
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn run_command(command: Command) -> Result<()> {
    match command {
        Command::Init {
            output,
            app_id,
            index,
        } => init_project(&output, app_id.as_deref(), index.as_deref())?,
        Command::Pack {
            input,
            output,
            app_id,
            platform,
            version,
            min_chunk,
            avg_chunk,
            max_chunk,
            max_pack_size,
            previous,
            release_tag,
            user_files,
            depot_key_hex,
            key_id,
        } => {
            let crypto = depot_key_hex
                .as_deref()
                .map(|key| CryptoContext::from_hex(key, key_id, app_id.clone()))
                .transpose()?;
            let manifest_crypto = crypto.clone();
            let chunking = ChunkingParams {
                min_size: min_chunk as u32,
                avg_size: avg_chunk as u32,
                max_size: max_chunk as u32,
            };
            let previous_paths = previous
                .iter()
                .map(|path| {
                    if path.is_dir() {
                        path.join("manifest.cbor.zst")
                    } else {
                        path.clone()
                    }
                })
                .collect::<Vec<_>>();
            let previous_manifests = previous_paths
                .iter()
                .map(|path| -> Result<Manifest> {
                    Manifest::decode(
                        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let previous_refs = previous_manifests
                .iter()
                .map(|manifest| vesper_format::PreviousBuild { manifest })
                .collect::<Vec<_>>();
            let mut result = vesper_format::build_from_directory_with_previous(
                &input,
                &output,
                app_id,
                platform,
                version,
                chunking,
                max_pack_size,
                crypto,
                &previous_refs,
            )?;
            if let Some(tag) = release_tag {
                for pack in &mut result.manifest.packs {
                    if pack.release_tag.is_none() {
                        pack.release_tag = Some(tag.clone());
                    }
                }
            }
            result.manifest.install.user_files = user_files;
            let manifest_bytes = if let Some(crypto) = manifest_crypto.as_ref() {
                result.manifest.encode_with_crypto(crypto)?
            } else {
                result.manifest.encode()?
            };
            fs::write(&result.manifest_path, manifest_bytes)?;
            println!(
                "manifest={} files={} chunks={} packs={}",
                result.manifest_path.display(),
                result.manifest.files.len(),
                result.manifest.chunks.len(),
                result.manifest.packs.len()
            );
        }
        Command::Check {
            build,
            input,
            max_file_size,
            json,
        } => {
            let input = build.or(input).context("build path is required")?;
            check_build(&input, max_file_size, json)?;
        }
        Command::DiffCheck {
            build,
            current,
            against,
            depot_key_hex,
            key_id,
            json,
        } => {
            let current = build
                .or(current)
                .context("current build path is required")?;
            diff_check(&current, &against, depot_key_hex.as_deref(), key_id, json)?;
        }
        Command::Publish {
            r2,
            metadata_dir,
            metadata_tag,
            tag,
            build,
            channel,
            index,
            dry_run,
            owner,
            repo,
            token,
            api_base,
            upload_base,
        } => publish_local(
            &build,
            tag.as_deref(),
            &channel,
            index.as_deref(),
            dry_run,
            PublishRemote {
                r2: r2.resolve()?,
                metadata_dir: metadata_dir.as_deref(),
                metadata_tag: &metadata_tag,
                owner: owner.as_deref(),
                repo: repo.as_deref(),
                token: token.as_deref(),
                api_base: api_base.as_deref(),
                upload_base: upload_base.as_deref(),
            },
        )?,
        Command::Promote { from, to, index } => promote_channel(&from, &to, &index)?,
        Command::Rollback { channel, index } => rollback_channel(&channel, &index)?,
        Command::InstallRollback { target } => {
            Installer::new(&target).rollback()?;
            println!("rolled back {}", target.display());
        }
        Command::Gc { index, yes } => gc_index(&index, yes)?,
        Command::Sign { targets, key_file } => sign_targets_file(&targets, key_file.as_deref())?,
        Command::Launch { target, seconds } => launch_installed(&target, seconds)?,
        Command::Index { command } => run_index_command(command)?,
        Command::Inspect {
            manifest,
            depot_key_hex,
            key_id,
        } => {
            let (manifest, _) =
                load_manifest_for_install(&manifest, depot_key_hex.as_deref(), key_id)?;
            manifest.validate()?;
            println!(
                "{} {} {} files={} chunks={} packs={}",
                manifest.app_id,
                manifest.platform,
                manifest.version,
                manifest.files.len(),
                manifest.chunks.len(),
                manifest.packs.len()
            );
        }
        Command::Verify {
            manifest,
            target,
            depot_key_hex,
            key_id,
        } => {
            let (manifest, _) =
                load_manifest_for_install(&manifest, depot_key_hex.as_deref(), key_id)?;
            let installer = Installer::new(&target);
            let invalid = installer.verify(&manifest)?;
            if invalid.is_empty() {
                println!("ok");
            } else {
                for path in &invalid {
                    if let Some(extra) = path.strip_prefix("extra: ") {
                        println!("extra: {extra}");
                    } else {
                        println!("invalid: {path}");
                    }
                }
                std::process::exit(2);
            }
        }
        Command::Install {
            manifest,
            sources,
            release,
            target,
            depot_key_hex,
            key_id,
            dry_run,
            json,
        } => {
            let (manifest, crypto) =
                load_manifest_for_install(&manifest, depot_key_hex.as_deref(), key_id)?;
            let specs = sources
                .into_iter()
                .map(|source| {
                    resolve_install_source(&source)
                        .and_then(|source| parse_source(&source))
                        .map(|source| {
                            SourceSpec::with_default_release(
                                source,
                                release.clone().unwrap_or_else(|| manifest.version.clone()),
                            )
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let fetcher =
                MultiSourceFetcher::new(specs)?.with_chunk_store(target.join(".vesper/chunks"))?;
            let fetcher = if let Some(crypto) = crypto {
                fetcher.with_crypto(crypto)
            } else {
                fetcher
            };
            let fetcher =
                if let Ok(bytes) = fs::read(target.join(".vesper/installed-manifest.cbor.zst")) {
                    if let Ok(previous) = Manifest::decode(&bytes) {
                        fetcher.with_local_manifest(&previous, &target)
                    } else {
                        fetcher
                    }
                } else {
                    fetcher
                };
            let installer = Installer::new(&target);
            if dry_run {
                let estimate = installer.preflight(&manifest, &fetcher)?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "changed_bytes": estimate.changed_bytes,
                            "download_bytes": estimate.download_bytes,
                            "peak_bytes": estimate.peak_bytes,
                            "hedged_bytes": 0,
                        })
                    );
                } else {
                    println!(
                        "changed_bytes={} download_bytes={} peak_bytes={}",
                        estimate.changed_bytes, estimate.download_bytes, estimate.peak_bytes
                    );
                }
                return Ok(());
            }
            installer.install(&manifest, &fetcher)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "installed": manifest.app_id,
                        "version": manifest.version,
                        "hedged_bytes": fetcher.hedged_bytes(),
                    })
                );
            } else {
                println!("installed {} {}", manifest.app_id, manifest.version);
            }
        }
        Command::Uninstall {
            manifest,
            target,
            delete_user_files,
        } => {
            let manifest = load_manifest(&manifest)?;
            Installer::new(&target).uninstall(&manifest, delete_user_files)?;
            println!("uninstalled {}", manifest.app_id);
        }
        Command::MetaInit { output } => meta_init(&output)?,
        Command::MetaVerify {
            root,
            targets,
            previous_targets_version,
            snapshot,
            timestamp,
        } => {
            let root_doc: Signed<RootSigned> = read_json(&root)?;
            let root_role = root_doc
                .signed
                .roles
                .get("root")
                .context("root role missing")?;
            verify_role(&root_doc, &root_doc.signed.keys, root_role)?;
            validate_version_and_expiry(
                root_doc.signed.version,
                root_doc.signed.expires,
                vesper_meta::now_unix(),
                None,
            )?;
            let targets_doc: Signed<TargetsSigned> = read_json(&targets)?;
            let targets_role = root_doc
                .signed
                .roles
                .get("targets")
                .context("targets role missing")?;
            verify_role(&targets_doc, &root_doc.signed.keys, targets_role)?;
            validate_version_and_expiry(
                targets_doc.signed.version,
                targets_doc.signed.expires,
                vesper_meta::now_unix(),
                previous_targets_version,
            )?;
            println!(
                "metadata ok: root={} targets={}",
                root_doc.signed.version, targets_doc.signed.version
            );
            if let Some(snapshot_path) = snapshot.as_ref() {
                let snapshot_doc: Signed<SnapshotSigned> = read_json(snapshot_path)?;
                let role = root_doc
                    .signed
                    .roles
                    .get("snapshot")
                    .context("snapshot role missing")?;
                verify_role(&snapshot_doc, &root_doc.signed.keys, role)?;
                validate_version_and_expiry(
                    snapshot_doc.signed.version,
                    snapshot_doc.signed.expires,
                    vesper_meta::now_unix(),
                    None,
                )?;
                let description = snapshot_doc
                    .signed
                    .meta
                    .get("targets.json")
                    .context("snapshot does not reference targets.json")?;
                verify_metadata_description(&targets, description)?;
                if description.version != targets_doc.signed.version {
                    anyhow::bail!("snapshot targets version does not match targets.json");
                }
                println!("snapshot ok: {}", snapshot_doc.signed.version);
            }
            if let Some(timestamp_path) = timestamp {
                let timestamp_doc: Signed<TimestampSigned> = read_json(&timestamp_path)?;
                let role = root_doc
                    .signed
                    .roles
                    .get("timestamp")
                    .context("timestamp role missing")?;
                verify_role(&timestamp_doc, &root_doc.signed.keys, role)?;
                validate_version_and_expiry(
                    timestamp_doc.signed.version,
                    timestamp_doc.signed.expires,
                    vesper_meta::now_unix(),
                    None,
                )?;
                let description = timestamp_doc
                    .signed
                    .meta
                    .get("snapshot.json")
                    .context("timestamp does not reference snapshot.json")?;
                let snapshot_path = snapshot
                    .as_ref()
                    .context("--snapshot is required with --timestamp")?;
                verify_metadata_description(snapshot_path, description)?;
                println!("timestamp ok: {}", timestamp_doc.signed.version);
            }
        }
        Command::MetaRefresh {
            dir,
            key_file,
            snapshot_key_file,
            timestamp_key_file,
            snapshot_version,
            timestamp_version,
        } => {
            let targets_path = dir.join("targets.json");
            let fallback_key = key_file.map(fs::read_to_string).transpose()?;
            let snapshot_secret = match snapshot_key_file {
                Some(path) => fs::read_to_string(path)?,
                None => fallback_key
                    .clone()
                    .context("--snapshot-key-file or --key-file is required")?,
            };
            let timestamp_secret = match timestamp_key_file {
                Some(path) => fs::read_to_string(path)?,
                None => fallback_key.context("--timestamp-key-file or --key-file is required")?,
            };
            let snapshot_key = KeyPair::from_secret_hex(&snapshot_secret)?;
            let timestamp_key = KeyPair::from_secret_hex(&timestamp_secret)?;
            let targets: Signed<TargetsSigned> = read_json(&targets_path)?;
            let snapshot =
                build_snapshot(&targets, snapshot_version, expires_in(7), &snapshot_key)?;
            let snapshot_path = dir.join("snapshot.json");
            write_json(&snapshot_path, &snapshot)?;
            let snapshot: Signed<SnapshotSigned> = read_json(&snapshot_path)?;
            let timestamp =
                build_timestamp(&snapshot, timestamp_version, expires_in(1), &timestamp_key)?;
            write_json(&dir.join("timestamp.json"), &timestamp)?;
            println!(
                "refreshed {} and {}",
                snapshot_path.display(),
                dir.join("timestamp.json").display()
            );
        }
        Command::MetaAddTarget {
            targets,
            key_file,
            name,
            file,
            app_id,
            platform,
            version,
            channel,
        } => {
            let document: Signed<TargetsSigned> = read_json(&targets)?;
            let key = vesper_meta::KeyPair::from_secret_hex(&fs::read_to_string(&key_file)?)?;
            let bytes = fs::read(&file)?;
            let mut custom = BTreeMap::new();
            custom.insert("app_id".into(), serde_json::json!(app_id));
            custom.insert("platform".into(), serde_json::json!(platform));
            custom.insert("version".into(), serde_json::json!(version));
            custom.insert("channel".into(), serde_json::json!(channel));
            let mut signed_targets = document.signed;
            signed_targets.version = signed_targets.version.saturating_add(1);
            signed_targets.expires = expires_in(90);
            signed_targets
                .targets
                .insert(name.clone(), vesper_meta::target_for_bytes(&bytes, custom));
            let updated = sign(signed_targets, &[&key])?;
            write_json(&targets, &updated)?;
            println!("added target {name} to {}", targets.display());
        }
        Command::PublishGithub {
            api_base,
            upload_base,
            index,
            channel,
            owner,
            repo,
            tag,
            manifest,
            packs_dir,
            token,
            depot_key_hex,
            key_id,
        } => {
            let token = token
                .or_else(|| token_from_env_or_gh().ok())
                .context("--token, GITHUB_TOKEN, or gh auth token is required")?;
            let (manifest_value, _) =
                load_manifest_for_install(&manifest, depot_key_hex.as_deref(), key_id)?;
            let mut publisher = GitHubPublisher::new(token)?;
            if api_base.is_some() || upload_base.is_some() {
                publisher = publisher.with_endpoints(
                    api_base.as_deref().unwrap_or("https://api.github.com"),
                    upload_base
                        .as_deref()
                        .unwrap_or("https://uploads.github.com"),
                );
            }
            let result = publisher.publish_manifest_and_packs(
                &owner,
                &repo,
                &tag,
                &manifest,
                &packs_dir,
                &manifest_value,
            )?;
            if let Some(index) = index {
                write_publish_index(
                    &index,
                    &channel,
                    &tag,
                    &manifest,
                    &manifest_value,
                    Some((&owner, &repo)),
                    None,
                )?;
            }
            println!(
                "release={} uploaded={} skipped_existing={}",
                result.release.tag_name,
                result.uploaded.len(),
                result.skipped_existing.len()
            );
        }
    }
    Ok(())
}

fn check_build(input: &PathBuf, max_file_size: u64, json: bool) -> Result<()> {
    let mut files = 0u64;
    let mut total = 0u64;
    let mut largest = (PathBuf::new(), 0u64);
    let mut oversized = Vec::new();
    let mut compressed_bytes = 0u64;
    for entry in walkdir::WalkDir::new(input).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            anyhow::bail!(
                "symbolic links are not supported: {}",
                entry.path().display()
            );
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(input)?
            .to_string_lossy()
            .replace('\\', "/");
        vesper_format::validate_relative_path(&relative)?;
        let size = entry.metadata()?.len();
        if size > 0 {
            let sample_len = size.min(64 * 1024) as usize;
            let mut sample = vec![0u8; sample_len];
            let mut file = fs::File::open(entry.path())?;
            let read = file.read(&mut sample)?;
            sample.truncate(read);
            let compressed = zstd::stream::encode_all(sample.as_slice(), 3)?;
            if compressed.len() >= sample.len() {
                compressed_bytes = compressed_bytes.saturating_add(size);
            }
        }
        files += 1;
        total = total.saturating_add(size);
        if size > largest.1 {
            largest = (entry.path().to_path_buf(), size);
        }
        if size > max_file_size {
            oversized.push((relative, size));
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({"files":files,"total_bytes":total,"largest":largest.0,"largest_bytes":largest.1,"peak_bytes":total.saturating_add(512*1024*1024),"compressed_ratio": if total == 0 { 0.0 } else { compressed_bytes as f64 / total as f64 }, "compression_sample_bytes": 65536})
        );
    } else {
        println!(
            "files={} total_bytes={} largest={} largest_bytes={} peak_bytes={} compressed_ratio={:.4}",
            files,
            total,
            largest.0.display(),
            largest.1,
            total.saturating_add(512 * 1024 * 1024),
            if total == 0 { 0.0 } else { compressed_bytes as f64 / total as f64 }
        );
    }
    for (path, size) in &oversized {
        println!("oversized path={path} bytes={size}");
    }
    if !oversized.is_empty() {
        anyhow::bail!("{} file(s) exceed max_file_size={max_file_size}; split them in the engine build before publishing", oversized.len());
    }
    Ok(())
}

fn diff_check(
    current: &PathBuf,
    against: &PathBuf,
    depot_key_hex: Option<&str>,
    key_id: Option<u64>,
    json: bool,
) -> Result<()> {
    let (current, _) = load_manifest_for_install(current, depot_key_hex, key_id)?;
    let (against, _) = load_manifest_for_install(against, depot_key_hex, key_id)?;
    let old_files: std::collections::BTreeMap<&str, &[u8; 32]> = against
        .files
        .iter()
        .map(|file| (file.path.as_str(), &file.content_hash))
        .collect();
    let mut changed_files = Vec::new();
    for file in &current.files {
        if old_files.get(file.path.as_str()).copied() != Some(&file.content_hash) {
            changed_files.push(file.path.clone());
        }
    }
    let old_chunks: std::collections::HashSet<_> = against.chunks.keys().copied().collect();
    let mut new_chunks = std::collections::HashSet::new();
    let mut new_bytes = 0u64;
    for file in &current.files {
        for chunk in &file.chunks {
            if !old_chunks.contains(&chunk.id) && new_chunks.insert(chunk.id) {
                new_bytes = new_bytes.saturating_add(chunk.raw_size);
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({"changed_files":changed_files,"new_chunks":new_chunks.len(),"new_raw_bytes":new_bytes})
        );
        return Ok(());
    }
    println!(
        "changed_files={} new_chunks={} new_raw_bytes={}",
        changed_files.len(),
        new_chunks.len(),
        new_bytes
    );
    for path in changed_files {
        println!("changed path={path}");
    }
    Ok(())
}

fn load_manifest(path: &PathBuf) -> Result<Manifest> {
    Manifest::decode(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
}

fn init_project(
    output: &PathBuf,
    app_id: Option<&str>,
    index: Option<&std::path::Path>,
) -> Result<()> {
    if output.exists() {
        anyhow::bail!("{} already exists", output.display());
    }
    let app_id = app_id.unwrap_or("com.example.game");
    let config = format!("app_id = \"{app_id}\"\nplatform = \"windows-x64\"\npaid = false\nlaunch = \"Game.exe\"\nuser_files = [\"Saved/**\"]\nchannel = \"stable\"\n");
    fs::write(output, config.as_bytes())?;
    let key = std::env::var("VESPER_DEV_KEY")
        .ok()
        .map(|value| KeyPair::from_secret_hex(&value))
        .transpose()?;
    let key = key.unwrap_or_else(KeyPair::generate);
    vesper_meta::store_developer_seed(app_id, &hex::encode(key.signing.to_bytes()))?;
    println!("app_id={app_id}\ndeveloper_public_key={}", key.public_hex());
    if let Some(index) = index {
        println!(
            "route-add: vesper index route-add --index {} --app {} --pubkey {}",
            index.display(),
            app_id,
            key.public_hex()
        );
    }
    Ok(())
}

fn developer_key(key_file: Option<&std::path::Path>) -> Result<KeyPair> {
    if let Some(path) = key_file {
        return KeyPair::from_secret_hex(&fs::read_to_string(path)?);
    }
    let app_id = std::env::var("VESPER_APP_ID")
        .ok()
        .or_else(|| {
            fs::read_to_string("vesper.toml").ok().and_then(|config| {
                config.lines().find_map(|line| {
                    line.strip_prefix("app_id = \"")
                        .and_then(|rest| rest.strip_suffix('"'))
                        .map(str::to_owned)
                })
            })
        })
        .unwrap_or_else(|| "com.example.game".into());
    let value = vesper_meta::load_developer_seed(&app_id)
        .context("developer key is required (set VESPER_DEV_KEY in CI or run vesper init)")?;
    KeyPair::from_secret_hex(&value)
}

struct PublishRemote<'a> {
    r2: Option<R2Config>,
    metadata_dir: Option<&'a std::path::Path>,
    metadata_tag: &'a str,
    owner: Option<&'a str>,
    repo: Option<&'a str>,
    token: Option<&'a str>,
    api_base: Option<&'a str>,
    upload_base: Option<&'a str>,
}

fn publish_local(
    build: &std::path::Path,
    tag: Option<&str>,
    channel: &str,
    index: Option<&std::path::Path>,
    dry_run: bool,
    remote: PublishRemote<'_>,
) -> Result<()> {
    let manifest_path = if build.is_file() {
        build.to_path_buf()
    } else {
        build.join("manifest.cbor.zst")
    };
    let manifest = load_manifest(&manifest_path)?;
    let tag = publish_tag(&manifest, tag)?;
    let packs_dir = manifest_path.parent().unwrap_or(build).join("packs");
    let mut bytes = 0u64;
    for pack in &manifest.packs {
        let path = packs_dir.join(&pack.pack_id);
        if path.is_file() {
            bytes = bytes.saturating_add(fs::metadata(path)?.len());
        }
    }
    let self_hosted = index.is_none() && remote.owner.is_some() && remote.repo.is_some();
    anyhow::ensure!(
        remote.owner.is_some() == remote.repo.is_some(),
        "--owner and --repo must be supplied together"
    );
    anyhow::ensure!(
        remote.metadata_dir.is_none() || self_hosted,
        "--metadata-dir requires --owner and --repo without --index"
    );
    if self_hosted {
        anyhow::ensure!(
            !remote.metadata_tag.trim().is_empty() && remote.metadata_tag != tag,
            "metadata tag must be nonempty and separate from the build tag"
        );
    }
    let r2 = remote.r2.clone().map(R2Publisher::from_env).transpose()?;
    anyhow::ensure!(
        r2.is_none() || remote.owner.zip(remote.repo).is_some(),
        "R2 publishing requires --owner and --repo"
    );
    if dry_run || (index.is_none() && !self_hosted) {
        println!(
            "publish dry-run app_id={} channel={} version={} packs={} bytes={}",
            manifest.app_id,
            channel,
            manifest.version,
            manifest.packs.len(),
            bytes
        );
        if let Some(r2) = &r2 {
            let github = github_publisher(&remote)?;
            r2.mirror(
                tag,
                &manifest_path,
                &packs_dir,
                &manifest,
                |release, pack, path| {
                    github.recover_pack(
                        remote.owner.unwrap(),
                        remote.repo.unwrap(),
                        release,
                        pack,
                        path,
                    )
                },
                true,
            )?;
            mirror_other_channels(
                r2,
                &github,
                &remote,
                index,
                &manifest,
                channel,
                tag,
                &manifest_path,
                &packs_dir,
                true,
            )?;
        }
        return Ok(());
    }
    if let (Some(owner), Some(repo)) = (remote.owner, remote.repo) {
        let publisher = github_publisher(&remote)?;
        let result = publisher.publish_manifest_and_packs(
            owner,
            repo,
            tag,
            &manifest_path,
            &packs_dir,
            &manifest,
        )?;
        println!(
            "github_release={} uploaded={} skipped_existing={}",
            result.release.tag_name,
            result.uploaded.len(),
            result.skipped_existing.len()
        );
        if let Some(r2) = &r2 {
            r2.mirror(
                tag,
                &manifest_path,
                &packs_dir,
                &manifest,
                |release, pack, path| publisher.recover_pack(owner, repo, release, pack, path),
                false,
            )?;
            mirror_other_channels(
                r2,
                &publisher,
                &remote,
                index,
                &manifest,
                channel,
                tag,
                &manifest_path,
                &packs_dir,
                false,
            )?;
        }
        if self_hosted {
            let dir = remote
                .metadata_dir
                .unwrap_or_else(|| std::path::Path::new("metadata"));
            let version = write_publish_metadata(
                dir,
                channel,
                tag,
                &manifest_path,
                &manifest,
                Some((owner, repo)),
                remote.r2.as_ref().map(R2Config::url_template).as_deref(),
            )?;
            publisher.publish_metadata(owner, repo, remote.metadata_tag, dir).with_context(||
                format!("local targets v{version} is signed but unpublished; rerun publish to publish it"))?;
            println!(
                "metadata_release={} targets_version={version} (keep {} for the next publication)",
                remote.metadata_tag,
                dir.display()
            );
            println!(
                "metadata_url=https://github.com/{owner}/{repo}/releases/download/{}/",
                remote.metadata_tag
            );
            return Ok(());
        }
    }
    write_publish_index(
        index.unwrap(),
        channel,
        tag,
        &manifest_path,
        &manifest,
        remote.owner.zip(remote.repo),
        remote.r2.as_ref().map(R2Config::url_template).as_deref(),
    )
}

/// All retained channels must be complete before the shared source is advertised.
#[allow(clippy::too_many_arguments)]
fn mirror_other_channels(
    r2: &R2Publisher,
    github: &GitHubPublisher,
    remote: &PublishRemote<'_>,
    index: Option<&std::path::Path>,
    manifest: &Manifest,
    current_channel: &str,
    current_tag: &str,
    manifest_path: &std::path::Path,
    packs_dir: &std::path::Path,
    dry_run: bool,
) -> Result<()> {
    let dir = index
        .map(|p| p.join("apps").join(&manifest.app_id))
        .unwrap_or_else(|| {
            remote
                .metadata_dir
                .unwrap_or_else(|| std::path::Path::new("metadata"))
                .to_path_buf()
        });
    let targets = dir.join("targets.json");
    if !targets.is_file() {
        return Ok(());
    }
    let targets: vesper_meta::Signed<AppTargetsSigned> = read_json(&targets)?;
    let mut inventory = BTreeMap::new();
    register_mirror_objects(&mut inventory, current_tag, manifest_path, manifest)?;
    for (name, channel) in &targets.signed.channels {
        if name == current_channel {
            continue;
        }
        let entry = &channel.manifest;
        if dry_run {
            r2.check_remote_manifest(
                &entry.release_tag,
                &entry.asset_name,
                entry.size,
                &entry.blake3,
            )?;
            github.recover_asset(
                remote.owner.unwrap(),
                remote.repo.unwrap(),
                &entry.release_tag,
                &entry.asset_name,
                &vesper_publish::r2::ObjectIdentity {
                    size: entry.size,
                    blake3: entry.blake3.clone(),
                },
                None,
            )?;
            continue;
        }
        let staging =
            tempfile::tempdir_in(manifest_path.parent().context("manifest parent missing")?)?;
        // Keep the original asset name for the mirror layout.
        anyhow::ensure!(
            std::path::Path::new(&entry.asset_name)
                .file_name()
                .and_then(|n| n.to_str())
                == Some(&entry.asset_name),
            "invalid channel manifest asset name"
        );
        let path = staging.path().join(&entry.asset_name);
        github.recover_asset(
            remote.owner.unwrap(),
            remote.repo.unwrap(),
            &entry.release_tag,
            &entry.asset_name,
            &vesper_publish::r2::ObjectIdentity {
                size: entry.size,
                blake3: entry.blake3.clone(),
            },
            Some(&path),
        )?;
        let other = load_manifest(&path)?;
        anyhow::ensure!(
            other.app_id == manifest.app_id,
            "channel manifest app id mismatch"
        );
        register_mirror_objects(&mut inventory, &entry.release_tag, &path, &other)?;
        r2.mirror(
            &entry.release_tag,
            &path,
            packs_dir,
            &other,
            |release, pack, path| {
                github.recover_pack(
                    remote.owner.unwrap(),
                    remote.repo.unwrap(),
                    release,
                    pack,
                    path,
                )
            },
            false,
        )?;
    }
    Ok(())
}

fn register_mirror_objects(
    inventory: &mut BTreeMap<(String, String), vesper_publish::r2::ObjectIdentity>,
    tag: &str,
    path: &std::path::Path,
    manifest: &Manifest,
) -> Result<()> {
    let mut objects = vec![(
        (
            tag.to_string(),
            path.file_name()
                .context("manifest filename missing")?
                .to_string_lossy()
                .into_owned(),
        ),
        vesper_publish::r2::ObjectIdentity {
            size: fs::metadata(path)?.len(),
            blake3: hex::encode(vesper_format::hash_file(path)?),
        },
    )];
    objects.extend(manifest.packs.iter().map(|pack| {
        (
            (
                pack.release_tag.as_deref().unwrap_or(tag).into(),
                pack.pack_id.clone(),
            ),
            vesper_publish::r2::ObjectIdentity {
                size: pack.size,
                blake3: hex::encode(pack.hash),
            },
        )
    }));
    for (key, identity) in objects {
        if let Some(previous) = inventory.insert(key.clone(), identity) {
            anyhow::ensure!(
                inventory[&key] == previous,
                "channels reference conflicting mirror content at {}/{}; use distinct release tags",
                key.0,
                key.1
            );
        }
    }
    Ok(())
}

fn github_publisher(remote: &PublishRemote<'_>) -> Result<GitHubPublisher> {
    let token = remote
        .token
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(token_from_env_or_gh)?;
    Ok(GitHubPublisher::new(token)?.with_endpoints(
        remote.api_base.unwrap_or("https://api.github.com"),
        remote.upload_base.unwrap_or("https://uploads.github.com"),
    ))
}

fn publish_tag<'a>(manifest: &'a Manifest, tag: Option<&'a str>) -> Result<&'a str> {
    if let Some(tag) = tag {
        anyhow::ensure!(!tag.trim().is_empty(), "--tag must not be empty");
        return Ok(tag);
    }
    if manifest.packs.iter().any(|pack| pack.release_tag.is_some())
        && !manifest
            .packs
            .iter()
            .any(|pack| pack.release_tag.as_deref() == Some(&manifest.version))
    {
        anyhow::bail!(
            "pack release tags do not include manifest version {}; use --tag <release>",
            manifest.version
        );
    }
    Ok(&manifest.version)
}

fn write_publish_index(
    index: &std::path::Path,
    channel: &str,
    tag: &str,
    manifest_path: &std::path::Path,
    manifest: &Manifest,
    github: Option<(&str, &str)>,
    r2_template: Option<&str>,
) -> Result<()> {
    let app_dir = index.join("apps").join(&manifest.app_id);
    write_publish_metadata(
        &app_dir,
        channel,
        tag,
        manifest_path,
        manifest,
        github,
        r2_template,
    )?;
    println!(
        "updated {} (upload packs with GitHub publisher, then commit this checkout)",
        app_dir.join("targets.json").display()
    );
    Ok(())
}

/// Signs the next targets version in `app_dir` and returns that version.
fn write_publish_metadata(
    app_dir: &std::path::Path,
    channel: &str,
    tag: &str,
    manifest_path: &std::path::Path,
    manifest: &Manifest,
    github: Option<(&str, &str)>,
    r2_template: Option<&str>,
) -> Result<u64> {
    fs::create_dir_all(app_dir)?;
    let targets_path = app_dir.join("targets.json");
    let metadata = vesper_meta::store_metadata(app_dir)?;
    let key = developer_key(None)?;
    let mut targets = if targets_path.exists() {
        read_json::<vesper_meta::Signed<AppTargetsSigned>>(&targets_path)?.signed
    } else {
        AppTargetsSigned {
            metadata: metadata.clone(),
            typ: "targets".into(),
            spec_version: "1.0.0".into(),
            version: 0,
            expires: expires_in(90),
            channels: BTreeMap::new(),
            sources: vec![AppSource {
                id: if github.is_some() {
                    "github".into()
                } else {
                    "local".into()
                },
                kind: if github.is_some() {
                    "github".into()
                } else {
                    "http".into()
                },
                url_template: "{release}/{pack_id}".into(),
                priority: 0,
            }],
        }
    };
    if let Some((owner, repo)) = github {
        targets.sources = vec![AppSource {
            id: "github".into(),
            kind: "github".into(),
            url_template: github_release_template(owner, repo),
            priority: 0,
        }];
    }
    if let Some(template) = r2_template {
        targets.sources.push(AppSource {
            id: "r2".into(),
            kind: "r2".into(),
            url_template: template.into(),
            priority: 1,
        });
    }
    targets.version = targets.version.saturating_add(1);
    targets.expires = expires_in(90);
    targets.channels.insert(
        channel.to_string(),
        AppChannel {
            version: targets.version,
            platform: manifest.platform.clone(),
            manifest: AppChannelManifest {
                release_tag: tag.to_owned(),
                asset_name: manifest_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                size: fs::metadata(manifest_path)?.len(),
                blake3: hex::encode(vesper_format::hash_bytes(&fs::read(manifest_path)?)),
            },
            encrypted: manifest.encrypted,
            key_id: manifest.key_id,
        },
    );
    targets.metadata = metadata;
    let version = targets.version;
    let signed = vesper_meta::sign(targets, &[&key])?;
    write_json(&targets_path, &signed)?;
    Ok(version)
}

fn promote_channel(from: &str, to: &str, index: &std::path::Path) -> Result<()> {
    let targets_path = single_targets_path(index)?;
    let mut doc: vesper_meta::Signed<AppTargetsSigned> = read_json(&targets_path)
        .with_context(|| format!("read targets {}", targets_path.display()))?;
    anyhow::ensure!(!doc.signed.sources.iter().any(|s| s.kind == "r2"),
        "promote with an R2 source requires mirror verification; use publish --channel <destination> with R2 settings to verify all channels");
    let channel = doc
        .signed
        .channels
        .get(from)
        .cloned()
        .context("source channel missing")?;
    doc.signed.version = doc.signed.version.saturating_add(1);
    doc.signed.channels.insert(to.into(), channel);
    doc.signed.metadata = vesper_meta::store_metadata(targets_path.parent().unwrap())?;
    let key = developer_key(None)?;
    write_json(&targets_path, &vesper_meta::sign(doc.signed, &[&key])?)
        .with_context(|| format!("write targets {}", targets_path.display()))?;
    Ok(())
}

fn rollback_channel(channel: &str, index: &std::path::Path) -> Result<()> {
    let targets_path = single_targets_path(index)?;
    let mut doc: vesper_meta::Signed<AppTargetsSigned> = read_json(&targets_path)
        .with_context(|| format!("read targets {}", targets_path.display()))?;
    let item = doc
        .signed
        .channels
        .get_mut(channel)
        .context("channel missing")?;
    item.version = item.version.saturating_sub(1);
    doc.signed.version = doc.signed.version.saturating_add(1);
    let key = developer_key(None)?;
    doc.signed.metadata = vesper_meta::store_metadata(targets_path.parent().unwrap())?;
    write_json(&targets_path, &vesper_meta::sign(doc.signed, &[&key])?)
        .with_context(|| format!("write targets {}", targets_path.display()))?;
    Ok(())
}

fn single_targets_path(index: &std::path::Path) -> Result<PathBuf> {
    let apps = index.join("apps");
    let mut targets = Vec::new();
    if apps.is_dir() {
        for entry in fs::read_dir(apps)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let path = entry.path().join("targets.json");
                if path.is_file() {
                    targets.push(path);
                }
            }
        }
    }
    match targets.len() {
        1 => Ok(targets.remove(0)),
        0 => anyhow::bail!(
            "no application targets.json found under {}",
            index.display()
        ),
        _ => anyhow::bail!("multiple applications found; use an app-specific promotion command"),
    }
}

fn gc_index(index: &std::path::Path, yes: bool) -> Result<()> {
    let packs = index.join("packs");
    let report = gc_report(&[], &packs)?;
    println!(
        "deletable_packs={} sparse_packs={}",
        report.deletable_packs.len(),
        report.sparse_packs.len()
    );
    if yes {
        for name in report.deletable_packs {
            fs::remove_file(packs.join(name))?;
        }
    }
    Ok(())
}

fn sign_targets_file(targets: &std::path::Path, key_file: Option<&std::path::Path>) -> Result<()> {
    let key = developer_key(key_file)?;
    let value: serde_json::Value = read_json(targets)?;
    if value["signed"].get("channels").is_some() {
        let mut document: Signed<AppTargetsSigned> = serde_json::from_value(value)?;
        document.signed.metadata =
            vesper_meta::store_metadata(targets.parent().context("targets directory missing")?)?;
        vesper_meta::validate_app_targets(&document.signed)?;
        write_json(targets, &vesper_meta::sign(document.signed, &[&key])?)?;
    } else {
        let document: Signed<TargetsSigned> = serde_json::from_value(value)?;
        write_json(targets, &vesper_meta::sign(document.signed, &[&key])?)?;
    }
    println!("signed {}", targets.display());
    Ok(())
}

fn launch_installed(target: &std::path::Path, seconds: u64) -> Result<()> {
    let installer = Installer::new(target);
    std::thread::sleep(Duration::from_secs(seconds));
    installer.cleanup_backup()?;
    println!("launch session complete; backups cleaned");
    Ok(())
}

fn run_index_command(command: IndexCommand) -> Result<()> {
    match command {
        IndexCommand::Init {
            index,
            root_keys,
            threshold,
            out_keys,
            online_pubkey,
        } => index_init(
            &index,
            root_keys,
            threshold,
            &out_keys,
            online_pubkey.as_deref(),
        ),
        IndexCommand::RootKeygen { out } => root::keygen(&out),
        IndexCommand::RootDraft(args) => root::draft(&args),
        IndexCommand::RootSign { draft, key_file } => root::sign(&draft, &key_file),
        IndexCommand::RootPublish { index, draft } => {
            let draft = draft.unwrap_or_else(|| root::default_draft(&index));
            root::publish(&index, &draft)
        }
        IndexCommand::OnlinePubkey => {
            println!(
                "{}",
                KeyPair::from_secret_hex(
                    &std::env::var("VESPER_ONLINE_KEY").context("VESPER_ONLINE_KEY is required")?
                )?
                .public_hex()
            );
            Ok(())
        }
        IndexCommand::RouteAdd {
            index,
            app,
            pubkey,
            metadata_url,
        } => index_route_add(&index, &app, &pubkey, metadata_url.as_deref()),
        IndexCommand::Aggregate { index } => index_aggregate(&index),
        IndexCommand::RouteSign {
            index,
            root_key_file,
        } => index_route_sign(&index, &root_key_file),
        IndexCommand::Verify {
            index,
            apps,
            previous,
            targets_only,
        } => index_verify(&index, &apps, previous.as_deref(), targets_only),
        IndexCommand::PendingSnapshots { index } => {
            let routes: Signed<RoutesSigned> = read_json(&index.join("routes.json"))?;
            for entry in fs::read_dir(index.join("apps"))? {
                let entry = entry?;
                if routes
                    .signed
                    .apps
                    .contains_key(&entry.file_name().to_string_lossy().to_string())
                    && entry.file_type()?.is_dir()
                    && snapshot_needed(&entry.path())?
                {
                    println!("{}", entry.file_name().to_string_lossy());
                }
            }
            Ok(())
        }
        IndexCommand::Snapshot {
            index,
            app,
            previous,
            initialize,
        } => index_snapshot(&index, &app, previous.as_deref(), initialize),
        IndexCommand::Timestamp {
            index,
            all,
            previous,
            initialize,
        } => index_timestamp(&index, all, previous.as_deref(), initialize),
    }
}

fn index_init(
    index: &std::path::Path,
    count: usize,
    threshold: u32,
    out_keys: &std::path::Path,
    online_pubkey: Option<&str>,
) -> Result<()> {
    if count == 0 || threshold == 0 || threshold as usize > count {
        anyhow::bail!("invalid root key threshold");
    }
    let online_pubkey = match online_pubkey {
        Some(public) => public.to_owned(),
        None => KeyPair::from_secret_hex(
            &std::env::var("VESPER_ONLINE_KEY")
                .context("--online-pubkey or VESPER_ONLINE_KEY is required")?,
        )?
        .public_hex(),
    };
    let online_bytes = hex::decode(&online_pubkey).context("online public key must be hex")?;
    anyhow::ensure!(
        online_bytes.len() == 32,
        "online public key must be 32 bytes"
    );
    let online_id = vesper_meta::public_key_id(&online_bytes)?;
    fs::create_dir_all(out_keys)?;
    fs::create_dir_all(index.join("apps"))?;
    let mut keys = BTreeMap::new();
    let mut keypairs = Vec::new();
    for n in 0..count {
        let key = KeyPair::generate();
        fs::write(
            out_keys.join(format!("root-{n}.key")),
            hex::encode(key.signing.to_bytes()),
        )?;
        keys.insert(key.keyid.clone(), key.public_hex());
        keypairs.push(key);
    }
    keys.insert(online_id.clone(), online_pubkey);
    let root = vesper_meta::RootSigned {
        typ: "root".into(),
        spec_version: "1.0.0".into(),
        version: 1,
        expires: expires_in(365),
        keys,
        roles: BTreeMap::from([
            (
                "root".into(),
                vesper_meta::RoleSpec {
                    keyids: keypairs.iter().map(|k| k.keyid.clone()).collect(),
                    threshold,
                },
            ),
            (
                "snapshot".into(),
                vesper_meta::RoleSpec {
                    keyids: vec![online_id.clone()],
                    threshold: 1,
                },
            ),
            (
                "timestamp".into(),
                vesper_meta::RoleSpec {
                    keyids: vec![online_id],
                    threshold: 1,
                },
            ),
        ]),
    };
    let signing_refs = keypairs.iter().take(threshold as usize).collect::<Vec<_>>();
    write_json(
        &index.join("root.json"),
        &vesper_meta::sign(root, &signing_refs)?,
    )?;
    let routes = RoutesSigned {
        typ: "routes".into(),
        spec_version: "1.0.0".into(),
        version: 1,
        expires: expires_in(365),
        apps: BTreeMap::new(),
    };
    write_json(
        &index.join("routes.json"),
        &vesper_meta::sign(routes, &signing_refs)?,
    )?;
    println!("initialized index {}", index.display());
    Ok(())
}

fn index_route_add(
    index: &std::path::Path,
    app: &str,
    pubkey: &str,
    metadata_url: Option<&str>,
) -> Result<()> {
    vesper_meta::hosting::validate_app_id(app)?;
    if let Some(url) = metadata_url {
        vesper_meta::hosting::metadata_base(url)?;
    }
    let path = index.join("routes.json");
    let mut doc: vesper_meta::Signed<RoutesSigned> = read_json(&path)?;
    let _: [u8; 32] = hex::decode(pubkey)
        .context("pubkey must be hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("pubkey must be 32 bytes"))?;
    let route = doc.signed.apps.entry(app.into()).or_insert(AppRoute {
        keys: Vec::new(),
        threshold: 1,
        metadata_url: None,
    });
    if let Some(url) = metadata_url {
        route.metadata_url = Some(url.into());
    }
    if !route.keys.contains(&pubkey.to_string()) {
        route.keys.push(pubkey.to_string());
    }
    doc.signed.version = doc.signed.version.saturating_add(1);
    doc.signatures.clear();
    write_json(&path, &doc)?;
    Ok(())
}

fn index_aggregate(index: &std::path::Path) -> Result<()> {
    let root: Signed<RootSigned> = read_json(&index.join("root.json"))?;
    verify_role(
        &root,
        &root.signed.keys,
        root.signed.roles.get("root").context("root role missing")?,
    )?;
    let routes: Signed<RoutesSigned> = read_json(&index.join("routes.json"))?;
    vesper_meta::verify_routes(&routes, &root)?;
    let mut report = String::new();
    for (id, route) in &routes.signed.apps {
        if route.metadata_url.is_none() {
            continue;
        }
        let result = match vesper_meta::hosting::aggregate_app(index, id, route) {
            Ok(status) => status.to_string(),
            Err(error) => format!("rejected: {error:#}"),
        };
        let line = format!("{id}: {result}");
        println!("{line}");
        report.push_str(&line);
        report.push('\n');
    }
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        // A code block prevents fetched error strings from becoming summary markup.
        let safe = report.replace('`', "'");
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
            "## Metadata aggregation\n\n```text\n{safe}```"
        )?;
    }
    Ok(())
}

fn index_route_sign(index: &std::path::Path, root_key_file: &std::path::Path) -> Result<()> {
    let path = index.join("routes.json");
    let doc: vesper_meta::Signed<RoutesSigned> = read_json(&path)?;
    let key = KeyPair::from_secret_hex(&fs::read_to_string(root_key_file)?)?;
    let mut signed = vesper_meta::sign(doc.signed, &[&key])?;
    signed.signatures.extend(doc.signatures);
    write_json(&path, &signed)?;
    Ok(())
}

fn index_verify(
    index: &std::path::Path,
    apps: &[String],
    previous: Option<&std::path::Path>,
    targets_only: bool,
) -> Result<()> {
    let root = root::verify_index_roots(index, previous)?;
    let routes: vesper_meta::Signed<RoutesSigned> = read_json(&index.join("routes.json"))?;
    vesper_meta::verify_routes(&routes, &root)?;
    if let Some(base) = previous {
        let old_routes: Signed<RoutesSigned> = read_json(&base.join("routes.json"))?;
        for app in old_routes.signed.apps.keys() {
            for name in ["snapshot.json", "timestamp.json"] {
                let old_path = base.join("apps").join(app).join(name);
                if old_path.exists() {
                    let old: serde_json::Value = read_json(&old_path)?;
                    let path = index.join("apps").join(app).join(name);
                    let new: serde_json::Value = read_json(&path).with_context(|| {
                        format!("previous {name} for {app} is missing from new index")
                    })?;
                    let version = |doc: &serde_json::Value| {
                        doc["signed"]["version"]
                            .as_u64()
                            .context("online metadata version missing")
                    };
                    anyhow::ensure!(
                        version(&new)? >= version(&old)?,
                        "{name} version rollback for {app}"
                    );
                }
            }
        }
    }
    let selected = if apps.is_empty() {
        routes.signed.apps.keys().cloned().collect()
    } else {
        apps.to_vec()
    };
    for app in selected {
        let route = routes
            .signed
            .apps
            .get(&app)
            .context("app missing from routes")?;
        let path = index.join("apps").join(&app).join("targets.json");
        // A registered hosted app enters the catalogue after its first valid aggregation.
        if apps.is_empty()
            && route.metadata_url.is_some()
            && !path.exists()
            && !index.join("apps").join(&app).exists()
        {
            continue;
        }
        let doc: vesper_meta::Signed<AppTargetsSigned> = read_json(&path)?;
        let old = if let Some(base) = previous {
            let old_path = base.join("apps").join(&app).join("targets.json");
            if old_path.exists() {
                Some(
                    read_json::<Signed<AppTargetsSigned>>(&old_path)?
                        .signed
                        .version,
                )
            } else {
                None
            }
        } else {
            None
        };
        vesper_meta::verify_app_targets(&doc, route, old)?;
        let dir = path.parent().context("app directory missing")?;
        vesper_meta::verify_store_metadata(&doc.signed, dir)
            .with_context(|| format!("application {app}"))?;
        if targets_only {
            continue;
        }
        let snapshot: Signed<SnapshotSigned> = read_json(&dir.join("snapshot.json"))?;
        let timestamp: Signed<TimestampSigned> = read_json(&dir.join("timestamp.json"))?;
        vesper_meta::verify_app_chain(&doc, &snapshot, &timestamp, &root)
            .with_context(|| format!("application {app}"))?;
    }
    println!("index ok");
    Ok(())
}

fn next_online_version(version: u64) -> Result<u64> {
    version
        .checked_add(1)
        .context("online metadata version overflow")
}

// Read every available version, including deleted files in full Git history.
// A shallow/no-history checkout must supply --previous or explicitly initialize.
fn online_version(
    index: &std::path::Path,
    app: &str,
    name: &str,
    previous: Option<&std::path::Path>,
    initialize: bool,
) -> Result<u64> {
    let relative = format!("apps/{app}/{name}");
    let mut versions = Vec::new();
    let mut record = |bytes: &[u8]| -> Result<()> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        versions.push(
            value["signed"]["version"]
                .as_u64()
                .context("online metadata version missing")?,
        );
        Ok(())
    };
    for dir in std::iter::once(index).chain(previous) {
        let path = dir.join(&relative);
        if path.exists() {
            record(&fs::read(path)?)?;
        }
    }
    if index.join(".git").exists() {
        let log = std::process::Command::new("git")
            .arg("-C")
            .arg(index)
            .args(["log", "--format=%H", "--", &relative])
            .output()?;
        anyhow::ensure!(
            log.status.success(),
            "cannot inspect online metadata history"
        );
        for commit in String::from_utf8(log.stdout)?.lines() {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(index)
                .args(["show", &format!("{commit}:{relative}")])
                .output()?;
            if output.status.success() {
                record(&output.stdout)?;
            }
        }
    }
    versions.into_iter().max().or(initialize.then_some(0))
        .with_context(|| format!("no previous {name} version for {app}; restore full Git history, supply --previous, or use --initialize for a new application"))
}

fn snapshot_needed(dir: &std::path::Path) -> Result<bool> {
    let targets: Signed<AppTargetsSigned> = read_json(&dir.join("targets.json"))?;
    let path = dir.join("snapshot.json");
    if !path.exists() {
        return Ok(true);
    }
    let snapshot: Signed<SnapshotSigned> = read_json(&path)?;
    let unchanged = snapshot
        .signed
        .meta
        .get("targets.json")
        .is_some_and(|description| {
            vesper_meta::verify_canonical_description(&targets, targets.signed.version, description)
                .is_ok()
        });
    Ok(!unchanged
        || snapshot
            .signed
            .expires
            .saturating_sub(vesper_meta::now_unix())
            < 2 * 86400)
}

fn index_snapshot(
    index: &std::path::Path,
    app: &str,
    previous: Option<&std::path::Path>,
    initialize: bool,
) -> Result<()> {
    let dir = index.join("apps").join(app);
    let targets: Signed<AppTargetsSigned> = read_json(&dir.join("targets.json"))?;
    let root: Signed<RootSigned> = read_json(&index.join("root.json"))?;
    let routes: Signed<RoutesSigned> = read_json(&index.join("routes.json"))?;
    vesper_meta::verify_routes(&routes, &root)?;
    let route = routes
        .signed
        .apps
        .get(app)
        .context("app missing from routes")?;
    vesper_meta::verify_app_targets(&targets, route, None)?;
    vesper_meta::verify_store_metadata(&targets.signed, &dir)?;
    // A full registry history distinguishes a hosted app's first aggregation
    // from deleted online metadata. Existing historical versions still win.
    let hosted_initial = route.metadata_url.is_some() && index.join(".git").exists() && {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(index)
            .args(["rev-parse", "--is-shallow-repository"])
            .output()?;
        output.status.success() && output.stdout == b"false\n"
    };
    let initialize = initialize || hosted_initial;
    let snapshot_path = dir.join("snapshot.json");
    let old_version = online_version(index, app, "snapshot.json", previous, initialize)?;
    let current_version = if snapshot_path.exists() {
        Some(
            read_json::<Signed<SnapshotSigned>>(&snapshot_path)?
                .signed
                .version,
        )
    } else {
        None
    };
    if !snapshot_needed(&dir)? && current_version == Some(old_version) {
        return Ok(());
    }
    let key = KeyPair::from_secret_hex(
        &std::env::var("VESPER_ONLINE_KEY").context("VESPER_ONLINE_KEY is required")?,
    )?;
    let payload = SnapshotSigned {
        typ: "snapshot".into(),
        spec_version: "1.0.0".into(),
        version: next_online_version(old_version)?,
        expires: expires_in(7),
        meta: BTreeMap::from([(
            "targets.json".into(),
            vesper_meta::metadata_description(&targets, targets.signed.version)?,
        )]),
    };
    let snapshot = vesper_meta::sign(payload, &[&key])?;
    // Prepare both documents before changing either file. A failed run is never pushed.
    let timestamp = renewed_timestamp(
        &snapshot,
        &key,
        online_version(index, app, "timestamp.json", previous, initialize)?,
    )?;
    write_json(&snapshot_path, &snapshot)?;
    write_json(&dir.join("timestamp.json"), &timestamp)?;
    Ok(())
}

fn renewed_timestamp(
    snapshot: &Signed<SnapshotSigned>,
    key: &KeyPair,
    old_version: u64,
) -> Result<Signed<TimestampSigned>> {
    build_timestamp(
        snapshot,
        next_online_version(old_version)?,
        expires_in(1),
        key,
    )
}

fn index_timestamp(
    index: &std::path::Path,
    all: bool,
    previous: Option<&std::path::Path>,
    initialize: bool,
) -> Result<()> {
    if !all {
        anyhow::bail!("--all is required");
    }
    let routes: Signed<RoutesSigned> = read_json(&index.join("routes.json"))?;
    let key = KeyPair::from_secret_hex(
        &std::env::var("VESPER_ONLINE_KEY").context("VESPER_ONLINE_KEY is required")?,
    )?;
    for entry in fs::read_dir(index.join("apps"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !routes
                .signed
                .apps
                .contains_key(&entry.file_name().to_string_lossy().to_string())
        {
            continue;
        }
        let dir = entry.path();
        let snapshot: Signed<SnapshotSigned> = read_json(&dir.join("snapshot.json"))?;
        let app = entry.file_name().to_string_lossy().into_owned();
        let timestamp = renewed_timestamp(
            &snapshot,
            &key,
            online_version(index, &app, "timestamp.json", previous, initialize)?,
        )?;
        write_json(&dir.join("timestamp.json"), &timestamp)?;
    }
    Ok(())
}

fn load_manifest_for_install(
    path: &PathBuf,
    depot_key_hex: Option<&str>,
    key_id: Option<u64>,
) -> Result<(Manifest, Option<CryptoContext>)> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.starts_with(vesper_format::MANIFEST_MAGIC) {
        let key = depot_key_hex.context("encrypted manifest requires --depot-key-hex")?;
        let (app_id, _version, manifest_key_id) = Manifest::encrypted_header(&bytes)?;
        let crypto = CryptoContext::from_hex(key, key_id.unwrap_or(manifest_key_id), app_id)?;
        let manifest = Manifest::decode_with_crypto(&bytes, &crypto)?;
        Ok((manifest, Some(crypto)))
    } else {
        let manifest = Manifest::decode(&bytes)?;
        let crypto = match depot_key_hex {
            Some(key) => Some(CryptoContext::from_hex(
                key,
                key_id.unwrap_or(manifest.key_id),
                manifest.app_id.clone(),
            )?),
            None => None,
        };
        Ok((manifest, crypto))
    }
}

fn parse_source(value: &str) -> Result<Source> {
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(Source::Http(value.to_string()))
    } else {
        Ok(Source::Local(PathBuf::from(value)))
    }
}

fn resolve_install_source(value: &str) -> Result<String> {
    if value.starts_with("http://") || value.starts_with("https://") {
        vesper_meta::validate_url_template(value)?;
        Ok(value.to_string())
    } else {
        Ok(value.to_string())
    }
}

fn github_release_template(owner: &str, repo: &str) -> String {
    format!("https://github.com/{owner}/{repo}/releases/download/{{release}}/{{pack_id}}")
}

fn meta_init(output: &PathBuf) -> Result<()> {
    fs::create_dir_all(output)?;
    let root_key = KeyPair::generate();
    let targets_key = KeyPair::generate();
    let snapshot_key = KeyPair::generate();
    let timestamp_key = KeyPair::generate();
    for (name, key) in [
        ("root", &root_key),
        ("targets", &targets_key),
        ("snapshot", &snapshot_key),
        ("timestamp", &timestamp_key),
    ] {
        fs::write(
            output.join(format!("{name}.key")),
            hex::encode(key.signing.to_bytes()),
        )?;
    }
    let mut keys = BTreeMap::new();
    for key in [&root_key, &targets_key, &snapshot_key, &timestamp_key] {
        keys.insert(key.keyid.clone(), key.public_hex());
    }
    let mut roles = BTreeMap::new();
    roles.insert(
        "root".to_string(),
        RoleSpec {
            keyids: vec![root_key.keyid.clone()],
            threshold: 1,
        },
    );
    roles.insert(
        "targets".to_string(),
        RoleSpec {
            keyids: vec![targets_key.keyid.clone()],
            threshold: 1,
        },
    );
    roles.insert(
        "snapshot".to_string(),
        RoleSpec {
            keyids: vec![snapshot_key.keyid.clone()],
            threshold: 1,
        },
    );
    roles.insert(
        "timestamp".to_string(),
        RoleSpec {
            keyids: vec![timestamp_key.keyid.clone()],
            threshold: 1,
        },
    );
    let root = RootSigned {
        typ: "root".into(),
        spec_version: "1.0.0".into(),
        version: 1,
        expires: expires_in(365),
        keys,
        roles,
    };
    let root_signed = sign(root, &[&root_key])?;
    write_json(&output.join("root.json"), &root_signed)?;
    let targets = TargetsSigned {
        typ: "targets".into(),
        spec_version: "1.0.0".into(),
        version: 1,
        expires: expires_in(90),
        targets: BTreeMap::new(),
        delegations: None,
    };
    let targets_signed = sign(targets, &[&targets_key])?;
    write_json(&output.join("targets.json"), &targets_signed)?;
    println!("created TUF bootstrap metadata in {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_init_binds_snapshot_and_timestamp_to_online_public_key() {
        let temp = tempfile::tempdir().unwrap();
        let key = KeyPair::generate();
        index_init(
            &temp.path().join("index"),
            1,
            1,
            &temp.path().join("keys"),
            Some(&key.public_hex()),
        )
        .unwrap();
        let root: vesper_meta::Signed<vesper_meta::RootSigned> =
            read_json(&temp.path().join("index/root.json")).unwrap();
        for role in ["snapshot", "timestamp"] {
            assert_eq!(root.signed.roles[role].threshold, 1);
            assert_eq!(root.signed.roles[role].keyids, vec![key.keyid.clone()]);
        }
        assert_eq!(root.signed.keys[&key.keyid], key.public_hex());
    }

    #[test]
    fn conflicting_channel_objects_require_distinct_release_tags() {
        let staging = tempfile::tempdir().unwrap();
        let path = staging.path().join("manifest.cbor.zst");
        fs::write(&path, b"first").unwrap();
        let manifest = Manifest::new(
            "demo".into(),
            "windows-x64".into(),
            "1".into(),
            vesper_format::ChunkingParams {
                min_size: 64,
                avg_size: 256,
                max_size: 1024,
            },
        );
        let mut inventory = BTreeMap::new();
        register_mirror_objects(&mut inventory, "r1", &path, &manifest).unwrap();
        fs::write(&path, b"other").unwrap();
        let error = register_mirror_objects(&mut inventory, "r1", &path, &manifest).unwrap_err();
        assert!(error.to_string().contains("conflicting mirror content"));
    }

    #[test]
    fn install_source_resolves_release_template() {
        assert_eq!(
            resolve_install_source("https://github.com/o/r/releases/download/{release}/{pack_id}")
                .unwrap(),
            "https://github.com/o/r/releases/download/{release}/{pack_id}"
        );
    }

    #[test]
    fn install_source_rejects_unknown_template_variables() {
        assert!(resolve_install_source("https://cdn.example/{branch}/{pack_id}").is_err());
    }

    #[test]
    fn github_publish_template_is_absolute_and_flat() {
        assert_eq!(
            github_release_template("owner", "repo"),
            "https://github.com/owner/repo/releases/download/{release}/{pack_id}"
        );
    }
}
