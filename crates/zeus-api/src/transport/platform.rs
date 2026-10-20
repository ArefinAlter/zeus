//! Small, isolated operating-system identity boundary.
#![allow(unsafe_code)]

use std::io;

/// Discard type-ahead on an already opened, verified controlling-terminal input handle.
pub fn discard_terminal_input(file: &std::fs::File) -> io::Result<()> {
    use std::io::IsTerminal;
    if !file.is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "controlling terminal required",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the caller owns a live verified terminal fd; TCIFLUSH only discards input.
        if unsafe { libc::tcflush(file.as_raw_fd(), libc::TCIFLUSH) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: borrowed live console handle; FlushConsoleInputBuffer does not close it.
        if unsafe {
            windows_sys::Win32::System::Console::FlushConsoleInputBuffer(file.as_raw_handle())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "terminal input unavailable",
        ))
    }
}

/// Protect an existing private application directory for the current user only (plus SYSTEM on
/// Windows). The caller must pass the exact application directory, never a shared parent directory.
/// Symlinks, reparse points and non-directories are refused.
pub fn secure_private_directory(path: &std::path::Path) -> io::Result<()> {
    secure_private_path(path, true)
}

/// Protect an existing private application file. Symlinks, reparse points and non-files are refused.
/// Unix additionally requires the current effective user to own the file.
pub fn secure_private_file(path: &std::path::Path) -> io::Result<()> {
    secure_private_path(path, false)
}

fn secure_private_path(path: &std::path::Path, directory: bool) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private path must have the expected regular type",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private path is owned by another user",
            ));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let current = file.metadata()?;
        if current.uid() != uid
            || current.dev() != metadata.dev()
            || current.ino() != metadata.ino()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private path changed while opening",
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(if directory {
            0o700
        } else {
            0o600
        }))
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                    SE_FILE_OBJECT, SetNamedSecurityInfoW,
                },
                DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
                PROTECTED_DACL_SECURITY_INFORMATION,
            },
            Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT,
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private path cannot be a reparse point",
            ));
        }
        let sid = user_identity()?;
        let inherit = if directory { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{inherit};FA;;;{sid})(A;{inherit};FA;;;SY)")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if name[..name.len() - 1].contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in private path",
            ));
        }
        // SAFETY: terminated UTF-16 inputs remain live, all output pointers reference initialized
        // locals. The OS-allocated descriptor is freed exactly once after the DACL has been applied.
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = std::ptr::null_mut();
            let result =
                if GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
                    == 0
                    || present == 0
                    || dacl.is_null()
                {
                    Err(io::Error::other("private DACL construction failed"))
                } else {
                    let error = SetNamedSecurityInfoW(
                        name.as_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        dacl,
                        std::ptr::null_mut(),
                    );
                    if error == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::from_raw_os_error(error as i32))
                    }
                };
            LocalFree(descriptor);
            result
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private filesystem permissions unavailable",
        ))
    }
}

#[cfg(unix)]
pub fn user_identity() -> io::Result<String> {
    // SAFETY: geteuid has no preconditions and returns the calling process's effective uid.
    Ok(unsafe { libc::geteuid() }.to_string())
}

#[cfg(windows)]
pub fn user_identity() -> io::Result<String> {
    process_identity(std::process::id())
}

#[cfg(windows)]
pub fn process_identity(pid: u32) -> io::Result<String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // SAFETY: handles are checked before wrapping, all buffers remain live for the FFI calls,
    // TOKEN_USER storage is usize-aligned, and the OS-allocated SID string is freed exactly once.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle::from_raw_handle(process);
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut size,
        );
        if size < std::mem::size_of::<TOKEN_USER>() as u32 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let info = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(info.User.Sid, &mut sid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(sid, len))
            .map_err(|_| io::Error::other("invalid OS identity"));
        LocalFree(sid.cast());
        result
    }
}

#[cfg(target_os = "linux")]
pub fn process_details(pid: u32) -> io::Result<(std::path::PathBuf, bool)> {
    let path = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after_name = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("invalid process stat"))?
        .1;
    // After comm: state, ppid, pgrp, session, tty_nr.
    let tty = after_name
        .split_whitespace()
        .nth(4)
        .and_then(|s| s.parse::<i64>().ok())
        .is_some_and(|n| n != 0);
    Ok((path, tty))
}

#[cfg(windows)]
pub fn process_details(pid: u32) -> io::Result<(std::path::PathBuf, bool)> {
    use std::os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    // SAFETY: OS handle is validated and owned, image buffer length is passed and checked by the OS.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = OwnedHandle::from_raw_handle(handle);
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        if QueryFullProcessImageNameW(handle.as_raw_handle(), 0, buffer.as_mut_ptr(), &mut len) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let path = std::ffi::OsString::from_wide(&buffer[..len as usize]).into();
        let mut session = 0;
        if ProcessIdToSessionId(pid, &mut session) == 0 {
            return Err(io::Error::last_os_error());
        }
        // Windows has no supported peer console query; a verified CLI asserts its CONIN$ check.
        Ok((path, session != 0))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn process_details(_pid: u32) -> io::Result<(std::path::PathBuf, bool)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer executable inspection unavailable",
    ))
}

#[cfg(test)]
mod private_path_tests {
    use super::*;
    #[test]
    fn private_paths_preserve_content_and_refuse_wrong_types() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        std::fs::write(&file, b"private-test").unwrap();
        secure_private_directory(dir.path()).unwrap();
        secure_private_file(&file).unwrap();
        secure_private_file(&file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"private-test");
        assert!(secure_private_directory(&file).is_err());
        assert!(secure_private_file(dir.path()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let link = dir.path().join("alias");
            symlink(&file, &link).unwrap();
            assert!(secure_private_file(&link).is_err());
        }
    }
    #[cfg(windows)]
    #[test]
    fn windows_acl_is_protected_and_has_only_two_explicit_aces() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
                DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
                SE_DACL_PROTECTED,
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, b"x").unwrap();
        secure_private_file(&path).unwrap();
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: OS allocations are checked and freed; ACL remains live with its descriptor.
        unsafe {
            let mut sd = std::ptr::null_mut();
            assert_eq!(
                GetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut sd
                ),
                0
            );
            let mut control = 0;
            let mut revision = 0;
            assert_ne!(
                GetSecurityDescriptorControl(sd, &mut control, &mut revision),
                0
            );
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            let mut present = 0;
            let mut defaulted = 0;
            let mut acl = std::ptr::null_mut();
            assert_ne!(
                GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut defaulted),
                0
            );
            assert_ne!(present, 0);
            assert!(!acl.is_null());
            assert_eq!((*acl).AceCount, 2);
            LocalFree(sd);
        }
    }
}
