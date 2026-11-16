#[cfg(not(windows))]
use crate::state::state_dir;
use anyhow::{Context, Result};
#[cfg(not(windows))]
use std::fs;
#[cfg(windows)]
use zeroize::Zeroize;

#[cfg(windows)]
pub(crate) fn credential_target() -> &'static str {
    crate::credentials::SESSION_TARGET
}

pub(crate) fn load_session_token() -> Result<String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Credentials::{
            CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        };
        let target: Vec<u16> = std::ffi::OsStr::new(credential_target())
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if ok == 0 || raw.is_null() {
            anyhow::bail!("Vesper account is not signed in")
        }
        let value = unsafe {
            let credential = &*raw;
            if credential.CredentialBlobSize == 0 || credential.CredentialBlob.is_null() {
                String::new()
            } else {
                let bytes = std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                String::from_utf8(bytes.to_vec()).context("invalid stored session")?
            }
        };
        unsafe {
            CredFree(raw as *mut std::ffi::c_void);
        }
        if value.is_empty() {
            anyhow::bail!("Vesper account is not signed in")
        }
        Ok(value)
    }
    #[cfg(not(windows))]
    {
        fs::read_to_string(state_dir().join("session.token"))
            .context("Vesper account is not signed in")
    }
}

pub(crate) fn store_session_token(token: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        };
        let target: Vec<u16> = std::ffi::OsStr::new(credential_target())
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut blob = token.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_ptr() as *mut u16,
            Comment: std::ptr::null_mut(),
            LastWritten: windows_sys::Win32::Foundation::FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: std::ptr::null_mut(),
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        blob.zeroize();
        if ok == 0 {
            anyhow::bail!("could not store Vesper session")
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::create_dir_all(state_dir())?;
        write_private(&state_dir().join("session.token"), token.as_bytes())
    }
}

pub(crate) fn delete_session_token() -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};
        let target: Vec<u16> = std::ffi::OsStr::new(credential_target())
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = fs::remove_file(state_dir().join("session.token"));
        Ok(())
    }
}

/// Write a file only its owner can read or write (0600 on Unix). Without a system
/// credential store wired in on these platforms, this keeps the session token
/// away from other local users.
#[cfg(not(windows))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).context("store Vesper session")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An older token file may have been created with wider permissions.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn session_file_is_private_even_if_it_existed_with_wider_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.token");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"new-token").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new-token");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
