use std::{io, path::Path};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

const RUN_VALUE_NAME: &str = "Xyra";

pub fn startup_command(executable: &Path) -> String {
    format!("\"{}\" --autostart", executable.display())
}

#[cfg(windows)]
pub fn sync_startup_registration(enabled: bool) -> io::Result<()> {
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
            RegOpenKeyExW, RegSetValueExW,
        },
    };

    let run_key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value_name = wide(RUN_VALUE_NAME);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: all pointers reference live, NUL-terminated UTF-16 buffers and `key` is valid
    // for the duration of these calls.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            run_key.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if opened != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(opened as i32));
    }

    let result = if enabled {
        let executable = std::env::current_exe()?;
        let command = wide(startup_command(&executable));
        // SAFETY: the registry handle is open and the byte count includes the UTF-16 NUL.
        unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * size_of::<u16>()) as u32,
            )
        }
    } else {
        // SAFETY: the registry handle is open and `value_name` is NUL terminated.
        unsafe { RegDeleteValueW(key, value_name.as_ptr()) }
    };
    // SAFETY: `key` was successfully opened above and is no longer used afterward.
    unsafe { RegCloseKey(key) };

    if result == ERROR_SUCCESS || (!enabled && result == ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result as i32))
    }
}

#[cfg(windows)]
fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

#[cfg(not(windows))]
pub fn sync_startup_registration(enabled: bool) -> io::Result<()> {
    if enabled {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "automatic startup is currently supported on Windows",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_command_quotes_paths_and_marks_the_launch() {
        assert_eq!(
            startup_command(Path::new(r"C:\Program Files\Xyra\xyra.exe")),
            r#""C:\Program Files\Xyra\xyra.exe" --autostart"#
        );
    }
}
