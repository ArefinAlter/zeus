//! The only terminal FFI: discard queued input on the verified controlling-terminal handle.
#![allow(unsafe_code)]

/// Restores terminal echo even when secret entry fails.
pub(crate) struct EchoGuard {
    file: std::fs::File,
    #[cfg(windows)]
    mode: u32,
    #[cfg(unix)]
    mode: libc::termios,
}
impl EchoGuard {
    pub fn disable(file: &std::fs::File) -> std::io::Result<Self> {
        let file = file.try_clone()?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Console::{
                ENABLE_ECHO_INPUT, GetConsoleMode, SetConsoleMode,
            };
            let mut mode = 0;
            // SAFETY: file owns a live CONIN$ handle; GetConsoleMode writes to a valid local.
            unsafe {
                if GetConsoleMode(file.as_raw_handle(), &mut mode) == 0
                    || SetConsoleMode(file.as_raw_handle(), mode & !ENABLE_ECHO_INPUT) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(Self { file, mode })
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: tcgetattr initializes the entire struct on success; file is a live /dev/tty.
            unsafe {
                if libc::tcgetattr(file.as_raw_fd(), mode.as_mut_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let mode = mode.assume_init();
                let mut hidden = mode;
                hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
                if libc::tcsetattr(file.as_raw_fd(), libc::TCSANOW, &hidden) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Self { file, mode })
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "secure terminal input unavailable",
            ))
        }
    }
}
impl Drop for EchoGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: owned live handle, restoring the mode read before secret entry.
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleMode(
                    self.file.as_raw_handle(),
                    self.mode,
                );
            }
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: owned live fd, initialized termios saved by tcgetattr.
            unsafe {
                libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.mode);
            }
        }
    }
}

pub(crate) fn discard_input(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the caller owns a live CONIN$ File opened for read/write. The call borrows it;
        // it never closes the handle or touches memory through a pointer.
        if unsafe {
            windows_sys::Win32::System::Console::FlushConsoleInputBuffer(file.as_raw_handle())
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the caller owns a live /dev/tty File. TCIFLUSH discards only terminal input.
        if unsafe { libc::tcflush(file.as_raw_fd(), libc::TCIFLUSH) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "terminal input flush unavailable",
        ))
    }
}
