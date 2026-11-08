use crate::{
    credentials,
    http::{client, join_url, put_json},
};
use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Fetch an authorized depot key for a paid application.
pub fn depot_key(gateway_base: &str, app_id: &str, key_id: u64, token: &str) -> Result<String> {
    fetch_depot_key_internal(gateway_base, app_id, key_id, token)
}
/// Redeem a code using the stored account session.
pub fn redeem(gateway_base: &str, app_id: &str, code: &str) -> Result<Value> {
    let token = credentials::load()?;
    client()?
        .post(join_url(gateway_base, &format!("apps/{app_id}/redeem")))
        .header("Cookie", format!("session={token}"))
        .json(&json!({"code": code}))
        .send()?
        .error_for_status()?
        .json()
        .map_err(Into::into)
}
/// Fetch grants using the stored account session.
pub fn grants(gateway_base: &str) -> Result<Value> {
    let token = credentials::load()?;
    Ok(client()?
        .get(join_url(gateway_base, "me/grants"))
        .header("Cookie", format!("session={token}"))
        .send()?
        .error_for_status()?
        .json()?)
}

fn fetch_depot_key_internal(
    gateway_base: &str,
    app_id: &str,
    key_id: u64,
    token: &str,
) -> Result<String> {
    let response = client()?
        .post(join_url(gateway_base, &format!("apps/{app_id}/key")))
        .header("Cookie", format!("session={token}"))
        .json(&json!({"key_id": key_id}))
        .send()?
        .error_for_status()?;
    let value: Value = response.json()?;
    value
        .get("depot_key_hex")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("gateway response omitted depot key")
}

/// Redeem an application code through the gateway.
pub fn redeem_code(gateway_base: &str, app_id: &str, code: &str) -> Result<Value> {
    redeem(gateway_base, app_id, code)
}

/// Request an email verification code from the gateway.
pub fn email_start(gateway_base: &str, email: &str) -> Result<()> {
    put_json(
        &join_url(gateway_base, "auth/email/start"),
        &json!({"email":email}),
    )
    .map(|_| ())
}

/// Verify the email code and store the returned account session.
pub fn email_verify(gateway_base: &str, email: &str, code: &str) -> Result<()> {
    let response = client()?
        .post(join_url(gateway_base, "auth/email/verify"))
        .json(&json!({"email":email,"code":code}))
        .send()?
        .error_for_status()?;
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .and_then(|v| v.split_once('=').map(|(_, value)| value.to_string()))
        .context("gateway did not return a session cookie")?;
    credentials::store(&cookie)
}

/// Fetch the signed-in account grants from the gateway.
pub fn fetch_grants(gateway_base: &str) -> Result<Value> {
    grants(gateway_base)
}

/// Delete the stored account session.
pub fn logout() -> Result<()> {
    credentials::delete()
}
