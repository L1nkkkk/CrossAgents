//! Child processes without a console window.
//!
//! The hub runs detached, without a console of its own. On Windows every console program it
//! starts (git, cmd, reg, a login tool) would then open a new console window that flashes on
//! the user's screen. `no_window` starts them without one; elsewhere it does nothing.

/// Starts the command without a console window (Windows' `CREATE_NO_WINDOW`).
pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

impl NoWindow for std::process::Command {
    #[cfg(windows)]
    fn no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_window(&mut self) -> &mut Self {
        self
    }
}

impl NoWindow for tokio::process::Command {
    #[cfg(windows)]
    fn no_window(&mut self) -> &mut Self {
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_window(&mut self) -> &mut Self {
        self
    }
}
