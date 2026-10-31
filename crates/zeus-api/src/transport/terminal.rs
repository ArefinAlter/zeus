//! Text output to a verified controlling terminal, independent of the Windows OEM code page.
#![allow(unsafe_code)]

use std::fs::File;
use std::io::{self, IsTerminal, Write};

/// UTF-8 text writer for a verified terminal; never redirects a human review to a file or pipe.
pub struct TerminalOutput(File);

impl TerminalOutput {
    /// Wrap an existing terminal handle, refusing redirected or detached output.
    pub fn new(file: File) -> io::Result<Self> {
        if !file.is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "controlling terminal required",
            ));
        }
        Ok(Self(file))
    }
}

impl Write for TerminalOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(not(windows))]
        {
            self.0.write(bytes)
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Console::WriteConsoleW;
            let text = std::str::from_utf8(bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            // Bound each allocation and native call, keeping complete UTF-8 characters.
            let mut end = text.len().min(8192);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let wide: Vec<u16> = text[..end].encode_utf16().collect();
            let mut remaining = wide.as_slice();
            while !remaining.is_empty() {
                let mut written = 0;
                // SAFETY: owned live console handle, initialized bounded UTF-16 slice and valid
                // output count. WriteConsoleW copies synchronously and retains no borrowed pointer.
                let ok = unsafe {
                    WriteConsoleW(
                        self.0.as_raw_handle(),
                        remaining.as_ptr().cast(),
                        remaining.len() as u32,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if written == 0 || written as usize > remaining.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "console made no valid progress",
                    ));
                }
                // Finish this chunk even if Windows splits a UTF-16 surrogate pair. Never report
                // half a UTF-8 character consumed, and abort the review if a native write fails.
                remaining = &remaining[written as usize..];
            }
            Ok(end)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        #[cfg(not(windows))]
        {
            self.0.flush()
        }
        #[cfg(windows)]
        {
            Ok(())
        } // Console writes above are synchronous and keep no buffered text.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_output_refuses_an_ordinary_file() {
        assert!(TerminalOutput::new(tempfile::tempfile().unwrap()).is_err());
    }

    #[cfg(windows)]
    mod windows_console {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/test-support/windows-console.rs"
        ));
    }

    #[test]
    #[cfg(windows)]
    fn unicode_review_ignores_legacy_code_page_and_refuses_detached_output() {
        windows_console::assert_console_gate(
            "transport::terminal::tests::unicode_review_ignores_legacy_code_page_and_refuses_detached_output",
            || {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::System::Console::{
                    CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo,
                    ReadConsoleOutputCharacterW, SetConsoleOutputCP,
                };
                let Ok(file) = File::options().read(true).write(true).open("CONOUT$") else {
                    return false;
                };
                let Ok(mut output) = TerminalOutput::new(file) else {
                    return false;
                };
                let handle = output.0.as_raw_handle();
                let mut before: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
                // SAFETY: the helper creates an isolated child console, never the user's console.
                // The live owned handle and output pointers remain valid throughout each call.
                assert_ne!(unsafe { SetConsoleOutputCP(437) }, 0);
                assert_ne!(
                    unsafe { GetConsoleScreenBufferInfo(handle, &mut before) },
                    0
                );
                let sample = "Café …";
                output.write_all(sample.as_bytes()).unwrap();
                output.flush().unwrap();
                let mut actual = [0u16; 6];
                let mut count = 0;
                assert_ne!(
                    unsafe {
                        ReadConsoleOutputCharacterW(
                            handle,
                            actual.as_mut_ptr(),
                            actual.len() as u32,
                            before.dwCursorPosition,
                            &mut count,
                        )
                    },
                    0
                );
                assert_eq!(count as usize, actual.len());
                assert_eq!(String::from_utf16(&actual).unwrap(), sample);
                true
            },
        );
    }
}
