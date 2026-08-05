use std::{ffi::OsStr, process::Command};

/// Creates a child process that never flashes a console window on Windows.
///
/// Xyra is a GUI application, but FFmpeg and Explorer are console-subsystem
/// executables. Without `CREATE_NO_WINDOW`, Windows briefly creates a terminal
/// for every probe, preview, export, and recording process.
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
