use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use winapi::ctypes::c_void;
use winapi::shared::ntdef::HANDLE;
use winapi::um::handleapi::{CloseHandle, DuplicateHandle, SetHandleInformation};
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::namedpipeapi::CreatePipe;
use winapi::um::processthreadsapi::GetCurrentProcess;
use winapi::um::winnt::DUPLICATE_SAME_ACCESS;

// handleapi.h (winapi 0.3.9 doesn't export this).
const HANDLE_FLAG_INHERIT: u32 = 0x00000001;

/// ConPTY signal pipe message: "resize the pseudoconsole window".
/// See `microsoft/terminal/src/winconpty/winconpty.h:49`.
/// Wire format (`winconpty.cpp:288-302`): `[8, cols_le, rows_le]`.
pub(crate) const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;

pub(crate) fn dup_handle(src: HANDLE) -> Option<RawHandle> {
    let mut dup: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            src,
            GetCurrentProcess(),
            &mut dup,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        None
    } else {
        Some(dup as RawHandle)
    }
}

/// Create an anonymous Win32 pipe with controllable per-end inheritance.
/// Matches the pattern recommended in the `CreatePipe` docs and used in
/// `microsoft/terminal`'s ConPTY setup: call with `bInheritHandle = TRUE`,
/// then selectively clear `HANDLE_FLAG_INHERIT` on the non-inheritable side.
pub fn create_anon_pipe(
    inherit_read: bool,
    inherit_write: bool,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };

    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();

    let ok = unsafe {
        CreatePipe(
            &mut read,
            &mut write,
            &mut sa as *mut SECURITY_ATTRIBUTES,
            0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let read_owned = unsafe { OwnedHandle::from_raw_handle(read as RawHandle) };
    let write_owned = unsafe { OwnedHandle::from_raw_handle(write as RawHandle) };

    if !inherit_read {
        let ok = unsafe {
            SetHandleInformation(read_owned.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if !inherit_write {
        let ok = unsafe {
            SetHandleInformation(
                write_owned.as_raw_handle() as HANDLE,
                HANDLE_FLAG_INHERIT,
                0,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok((read_owned, write_owned))
}

pub(crate) struct HandleReader {
    pub(crate) handle: Option<OwnedHandle>,
}

impl Read for HandleReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let h = self
            .handle
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "handle closed"))?;
        use winapi::um::fileapi::ReadFile;
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                h.as_raw_handle() as *mut c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(bytes_read as usize)
        }
    }
}

pub(crate) struct HandleWriter {
    pub(crate) handle: Option<RawHandle>,
}

unsafe impl Send for HandleWriter {}

impl Write for HandleWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let h = self
            .handle
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "handle closed"))?;
        use winapi::um::fileapi::WriteFile;
        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                h as *mut c_void,
                buf.as_ptr() as *const c_void,
                buf.len() as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(bytes_written as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for HandleWriter {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            unsafe {
                CloseHandle(h as HANDLE);
            }
        }
    }
}
