//! Which package platforms this computer runs, and which published channel to install.
use anyhow::Result;
use std::collections::BTreeMap;
use vesper_meta::AppChannel;

/// Package platforms this build of the client can run, best first. Apple Silicon
/// Macs also accept Intel builds, which run under Rosetta.
pub fn host_platforms() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["windows-x64"]
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        &["macos-arm64", "macos-x64"]
    } else if cfg!(target_os = "macos") {
        &["macos-x64"]
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        &["linux-x64"]
    } else {
        &[]
    }
}

/// Choose the published channel to install for `requested` on a computer that runs
/// `hosts`. Developers publish one channel per platform as `<channel>-<platform>`
/// (for example `stable-macos-arm64`); a plain `<channel>` is used when its own
/// platform matches. Returns the channel name to install and remember.
pub fn resolve_channel(
    channels: &BTreeMap<String, AppChannel>,
    requested: &str,
    hosts: &[&str],
) -> Result<String> {
    for host in hosts {
        let name = format!("{requested}-{host}");
        if channels
            .get(&name)
            .is_some_and(|channel| channel.platform == *host)
        {
            return Ok(name);
        }
    }
    match channels.get(requested) {
        Some(channel) if hosts.contains(&channel.platform.as_str()) => Ok(requested.to_string()),
        Some(_) | None => {
            let published: Vec<&str> = channels
                .iter()
                .filter(|(name, _)| {
                    name.as_str() == requested || name.starts_with(&format!("{requested}-"))
                })
                .map(|(_, channel)| channel.platform.as_str())
                .collect();
            if published.is_empty() {
                anyhow::bail!("requested channel is not published")
            }
            anyhow::bail!(
                "this game has no {requested} build for this computer ({}); published for: {}",
                hosts.first().copied().unwrap_or("unsupported platform"),
                published.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vesper_meta::AppChannelManifest;

    fn channel(platform: &str) -> AppChannel {
        AppChannel {
            version: 1,
            platform: platform.into(),
            manifest: AppChannelManifest {
                release_tag: "v1".into(),
                asset_name: "manifest.cbor.zst".into(),
                size: 1,
                blake3: "00".repeat(32),
            },
            encrypted: false,
            key_id: 0,
        }
    }

    fn channels(entries: &[(&str, &str)]) -> BTreeMap<String, AppChannel> {
        entries
            .iter()
            .map(|(name, platform)| (name.to_string(), channel(platform)))
            .collect()
    }

    #[test]
    fn windows_keeps_plain_stable_channel() {
        let published = channels(&[("stable", "windows-x64"), ("stable-linux-x64", "linux-x64")]);
        assert_eq!(
            resolve_channel(&published, "stable", &["windows-x64"]).unwrap(),
            "stable"
        );
    }

    #[test]
    fn platform_channel_wins_over_plain_channel() {
        let published = channels(&[("stable", "windows-x64"), ("stable-linux-x64", "linux-x64")]);
        assert_eq!(
            resolve_channel(&published, "stable", &["linux-x64"]).unwrap(),
            "stable-linux-x64"
        );
    }

    #[test]
    fn apple_silicon_falls_back_to_intel_build() {
        let published = channels(&[("stable", "windows-x64"), ("stable-macos-x64", "macos-x64")]);
        let hosts = ["macos-arm64", "macos-x64"];
        assert_eq!(
            resolve_channel(&published, "stable", &hosts).unwrap(),
            "stable-macos-x64"
        );
        let both = channels(&[
            ("stable-macos-arm64", "macos-arm64"),
            ("stable-macos-x64", "macos-x64"),
        ]);
        assert_eq!(
            resolve_channel(&both, "stable", &hosts).unwrap(),
            "stable-macos-arm64"
        );
    }

    #[test]
    fn stored_platform_channel_resolves_to_itself() {
        let published = channels(&[("stable-linux-x64", "linux-x64")]);
        assert_eq!(
            resolve_channel(&published, "stable-linux-x64", &["linux-x64"]).unwrap(),
            "stable-linux-x64"
        );
    }

    #[test]
    fn missing_platform_names_what_is_published() {
        let published = channels(&[("stable", "windows-x64")]);
        let error = resolve_channel(&published, "stable", &["linux-x64"])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("no stable build for this computer (linux-x64)"),
            "{error}"
        );
        assert!(error.contains("published for: windows-x64"), "{error}");
        let error = resolve_channel(&published, "beta", &["windows-x64"])
            .unwrap_err()
            .to_string();
        assert_eq!(error, "requested channel is not published");
    }

    #[test]
    fn a_misnamed_platform_channel_is_not_used() {
        // A channel named for Linux but carrying a Windows build must not be installed on Linux.
        let published = channels(&[("stable-linux-x64", "windows-x64")]);
        assert!(resolve_channel(&published, "stable", &["linux-x64"]).is_err());
    }
}
