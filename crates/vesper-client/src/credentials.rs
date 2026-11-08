use anyhow::Result;
/// The Windows Credential Manager session target.
pub const SESSION_TARGET: &str = "Vesper.Launcher.Session";
/// Read the stored account session using the platform service.
pub fn load() -> Result<String> {
    crate::platform::credentials::load_session_token()
}
/// Persist the account session using the platform service.
pub fn store(token: &str) -> Result<()> {
    crate::platform::credentials::store_session_token(token)
}
/// Delete the stored account session using the platform service.
pub fn delete() -> Result<()> {
    crate::platform::credentials::delete_session_token()
}
