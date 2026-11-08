use anyhow::Result;
use serde_json::{json, Value};
use vesper_format::Manifest;

use serde::Serialize;
/// An official runtime installer and its manual verification status.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeEntry {
    /// Runtime prerequisite name used by manifests.
    pub name: &'static str,
    /// Official installer download URL.
    pub official_url: &'static str,
    /// Manually verified official installer digest, when available.
    pub sha256: Option<&'static str>,
    /// Whether the runtime installer is enabled.
    pub enabled: bool,
    /// Reason the runtime installer remains disabled.
    pub disabled_reason: &'static str,
}
/// Return the official runtime entries and their verification status.
pub fn allowlist() -> Vec<RuntimeEntry> {
    vec![
        RuntimeEntry {
            name: "VC++ 2015-2022 x64",
            official_url: "https://aka.ms/vs/17/release/vc_redist.x64.exe",
            sha256: None,
            enabled: false,
            disabled_reason: "official installer hash requires manual verification",
        },
        RuntimeEntry {
            name: "DirectX June 2010",
            official_url: "https://www.microsoft.com/en-us/download/details.aspx?id=8109",
            sha256: None,
            enabled: false,
            disabled_reason: "official installer hash requires manual verification",
        },
    ]
}
/// Check whether a runtime has an enabled, verified installer hash.
pub fn is_allowed(name: &str) -> bool {
    allowlist()
        .iter()
        .any(|entry| entry.name == name && entry.enabled && entry.sha256.is_some())
}

/// Reject manifest runtime prerequisites without a verified allowlist entry.
pub fn validate_runtime_rules(manifest: &Manifest) -> Result<()> {
    if let Some(runtime) = manifest.install.runtimes.first() {
        if is_allowed(&runtime.name) {
            return Ok(());
        }
        anyhow::bail!(
            "runtime prerequisite '{}' is disabled until its official installer hash is manually verified",
            runtime.name
        )
    }
    Ok(())
}

/// Serialize official runtime entries for frontend consumption.
pub fn runtime_allowlist() -> Vec<Value> {
    allowlist()
        .into_iter()
        .map(|entry| serde_json::to_value(entry).unwrap_or_else(|_| json!({})))
        .collect()
}
