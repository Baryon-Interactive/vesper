use crate::http::{get_json, join_url};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Fetch the first static catalogue page.
pub fn fetch(api_base: &str) -> Result<StoreResponse> {
    let value: Value = get_json(&join_url(api_base, "api/v1/apps/page-1.json"))?;
    let mut apps = Vec::new();
    for item in value
        .get("apps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let app_id = item
            .get("app_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("catalogue application id missing"))?;
        let verified = detail(api_base, app_id)?;
        apps.push(AppSummary {
            app_id: app_id.into(),
            store: verified["store"].clone(),
        });
    }
    Ok(StoreResponse {
        total: value
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(apps.len() as u64) as usize,
        apps,
    })
}
/// Fetch published application details from the static catalogue.
pub fn detail(api_base: &str, app_id: &str) -> Result<Value> {
    anyhow::ensure!(
        !app_id.is_empty()
            && app_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && app_id != "."
            && app_id != "..",
        "invalid catalogue application id"
    );
    let (targets, _) = crate::metadata::verify_app(api_base, app_id)?;
    let mut files = Vec::new();
    for name in ["store.json", "description.md"] {
        let bytes = crate::http::client()?
            .get(join_url(api_base, &format!("apps/{app_id}/{name}")))
            .send()?
            .error_for_status()?
            .bytes()?;
        vesper_meta::verify_store_bytes(&targets.signed, name, &bytes)?;
        files.push(bytes);
    }
    Ok(serde_json::json!({
        "app_id": app_id,
        "store": serde_json::from_slice::<Value>(&files[0])?,
        "description": std::str::from_utf8(&files[1])?,
    }))
}

/// Application identifier and its published store metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    /// Published application identifier.
    pub app_id: String,
    /// Published store metadata, unchanged from the index.
    pub store: Value,
}

/// A catalogue page and its total application count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreResponse {
    /// Total application count reported by the catalogue.
    pub total: usize,
    /// Applications included in this catalogue page.
    pub apps: Vec<AppSummary>,
}

/// Fetch the first static catalogue page.
pub fn fetch_store(api_base: &str) -> Result<StoreResponse> {
    fetch(api_base)
}

/// Fetch published application details from the static catalogue.
pub fn fetch_app_detail(api_base: &str, app_id: &str) -> Result<Value> {
    detail(api_base, app_id)
}
