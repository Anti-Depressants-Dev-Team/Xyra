use std::io;

pub const YOUTUBE_REFRESH_TOKEN: &str = "Xyra/YouTubeRefreshToken";
pub const YOUTUBE_CLIENT_SECRET: &str = "Xyra/YouTubeClientSecret";

#[cfg(windows)]
pub fn write_secret(target: &str, secret: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
    };

    let mut target = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut username = "Xyra".encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut blob = secret.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_mut_ptr(),
        ..Default::default()
    };
    if unsafe { CredWriteW(&credential, 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub fn read_secret(target: &str) -> io::Result<Option<String>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };

    const ERROR_NOT_FOUND: i32 = 1168;
    let target = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) } == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NOT_FOUND) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if raw.is_null() {
        return Ok(None);
    }
    let credential = unsafe { &*raw };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            credential.CredentialBlob,
            credential.CredentialBlobSize as usize,
        )
    };
    let value = String::from_utf8(bytes.to_vec())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    unsafe { CredFree(raw.cast()) };
    value
}

#[cfg(windows)]
pub fn delete_secret(target: &str) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};

    const ERROR_NOT_FOUND: i32 = 1168;
    let target = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NOT_FOUND) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn write_secret(_target: &str, _secret: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure account storage is currently supported on Windows",
    ))
}

#[cfg(not(windows))]
pub fn read_secret(_target: &str) -> io::Result<Option<String>> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn delete_secret(_target: &str) -> io::Result<()> {
    Ok(())
}
