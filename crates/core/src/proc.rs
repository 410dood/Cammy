//! Keep spawned child processes (go2rtc, ffmpeg) from popping a console window
//! on Windows. The packaged desktop app is a GUI process with no console of its
//! own, so a console-subsystem child (go2rtc.exe, ffmpeg.exe) would otherwise
//! allocate its own window and flash a black terminal on screen — confusing for
//! someone who just installed the app.

use std::process::Command;

/// Adds `CREATE_NO_WINDOW` to a command on Windows; a no-op on other platforms.
pub(crate) trait NoConsole {
    fn no_console(&mut self) -> &mut Self;
}

impl NoConsole for Command {
    #[cfg(windows)]
    fn no_console(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the child neither inherits nor allocates a console.
        self.creation_flags(0x0800_0000)
    }
    #[cfg(not(windows))]
    fn no_console(&mut self) -> &mut Self {
        self
    }
}
