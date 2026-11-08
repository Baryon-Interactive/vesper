use anyhow::Result;
use reqwest::blocking::Client;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(format!(
            "vesper/{} {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        ))
        .timeout(Duration::from_secs(30))
        .build()?)
}

pub(crate) fn get_json<T: DeserializeOwned>(url: &str) -> Result<T> {
    let response = client()?.get(url).send()?.error_for_status()?;
    Ok(response.json()?)
}

pub(crate) fn put_json<T: Serialize>(url: &str, value: &T) -> Result<Value> {
    let response = client()?.post(url).json(value).send()?.error_for_status()?;
    Ok(response.json().unwrap_or_else(|_| json!({})))
}

pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}
