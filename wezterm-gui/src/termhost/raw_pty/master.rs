use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Mutex;

use portable_pty::{MasterPty, PtySize};
use winapi::ctypes::c_void;
use winapi::shared::ntdef::HANDLE;
use winapi::um::handleapi::CloseHandle;

use super::io::{dup_handle, HandleReader, HandleWriter, PTY_SIGNAL_RESIZE_WINDOW};

/// `MasterPty` backed by raw Win32 file handles from the termhost handoff.
pub struct RawHandlesMasterPty {
    read: Mutex<Option<OwnedHandle>>,
    write: Mutex<Option<RawHandle>>,
    signal: Mutex<Option<RawHandle>>,
    reference: Mutex<Option<RawHandle>>,
    size: Mutex<PtySize>,
}

unsafe impl Send for RawHandlesMasterPty {}

impl std::fmt::Debug for RawHandlesMasterPty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawHandlesMasterPty")
            .field(
                "read",
                &self
                    .read
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|h| h.as_raw_handle()),
            )
            .field("size", &self.size.lock().unwrap())
            .finish()
    }
}

impl RawHandlesMasterPty {
    /// # Safety
    ///
    /// All four handles must be valid Win32 handles owned by the caller.
    /// The caller must not use them after this call.
    pub unsafe fn from_raw_handles(
        read: HANDLE,
        write: HANDLE,
        signal: HANDLE,
        reference: HANDLE,
        initial_size: PtySize,
    ) -> Self {
        let read_owned = if read.is_null() {
            None
        } else {
            Some(OwnedHandle::from_raw_handle(read as RawHandle))
        };
        let write_owned = if write.is_null() {
            None
        } else {
            Some(OwnedHandle::from_raw_handle(write as RawHandle))
        };

        // Duplicate `write` so take_writer can move it without disturbing
        // write_owned's Drop.
        let write = if let Some(owned) = write_owned.as_ref() {
            match dup_handle(owned.as_raw_handle() as HANDLE) {
                Some(h) => Some(h),
                None => {
                    log::warn!(
                        "wezterm-termhost: DuplicateHandle for write pipe failed: {}",
                        io::Error::last_os_error()
                    );
                    None
                }
            }
        } else {
            None
        };

        let signal_h = if signal.is_null() {
            None
        } else {
            match dup_handle(signal) {
                Some(h) => Some(h),
                None => {
                    log::warn!(
                        "wezterm-termhost: DuplicateHandle for signal failed: {}",
                        io::Error::last_os_error()
                    );
                    None
                }
            }
        };

        let reference_h = if reference.is_null() {
            None
        } else {
            match dup_handle(reference) {
                Some(h) => Some(h),
                None => {
                    log::warn!(
                        "wezterm-termhost: DuplicateHandle for reference failed: {}",
                        io::Error::last_os_error()
                    );
                    None
                }
            }
        };

        Self {
            read: Mutex::new(read_owned),
            write: Mutex::new(write),
            signal: Mutex::new(signal_h),
            reference: Mutex::new(reference_h),
            size: Mutex::new(initial_size),
        }
    }

    #[cfg(test)]
    pub fn signal_handle(&self) -> Option<RawHandle> {
        self.signal.lock().unwrap().clone()
    }

    #[cfg(test)]
    pub fn reference_handle(&self) -> Option<RawHandle> {
        self.reference.lock().unwrap().clone()
    }
}

impl Drop for RawHandlesMasterPty {
    fn drop(&mut self) {
        if let Some(h) = self.write.lock().unwrap().take() {
            unsafe {
                CloseHandle(h as HANDLE);
            }
        }
        if let Some(h) = self.signal.lock().unwrap().take() {
            unsafe {
                CloseHandle(h as HANDLE);
            }
        }
        if let Some(h) = self.reference.lock().unwrap().take() {
            unsafe {
                CloseHandle(h as HANDLE);
            }
        }
    }
}

impl MasterPty for RawHandlesMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        // Wire format per PTY_SIGNAL_RESIZE_WINDOW (winconpty.cpp:288-302).
        let signal_handle = self
            .signal
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("signal pipe handle not available"))?;

        let id_bytes = PTY_SIGNAL_RESIZE_WINDOW.to_le_bytes();
        let col_bytes = size.cols.to_le_bytes();
        let row_bytes = size.rows.to_le_bytes();
        let bytes: [u8; 6] = [
            id_bytes[0],
            id_bytes[1],
            col_bytes[0],
            col_bytes[1],
            row_bytes[0],
            row_bytes[1],
        ];

        let mut bytes_written: u32 = 0;
        let ok = unsafe {
            winapi::um::fileapi::WriteFile(
                signal_handle as HANDLE,
                bytes.as_ptr() as *const c_void,
                bytes.len() as u32,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(anyhow::anyhow!(
                "WriteFile to ConPTY signal pipe failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        if bytes_written != bytes.len() as u32 {
            return Err(anyhow::anyhow!(
                "short write to ConPTY signal pipe: wrote {} of {} bytes",
                bytes_written,
                bytes.len()
            ));
        }

        *self.size.lock().unwrap() = size;
        Ok(())
    }

    fn get_size(&self) -> anyhow::Result<PtySize> {
        Ok(*self.size.lock().unwrap())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        let guard = self.read.lock().unwrap();
        let owned = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("read handle already taken"))?;
        let raw = owned.as_raw_handle();
        let dup = dup_handle(raw as HANDLE).ok_or_else(|| io::Error::last_os_error())?;
        let new_owned = unsafe { OwnedHandle::from_raw_handle(dup) };
        Ok(Box::new(HandleReader {
            handle: Some(new_owned),
        }))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        let raw = self
            .write
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow::anyhow!("writer already taken"))?;
        Ok(Box::new(HandleWriter { handle: Some(raw) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_handles_master_pty_handles_null_inputs() {
        let pty = unsafe {
            RawHandlesMasterPty::from_raw_handles(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                PtySize::default(),
            )
        };
        assert!(pty.signal_handle().is_none());
        assert!(pty.reference_handle().is_none());
        assert!(pty.take_writer().is_err());
    }

    /// P-sig: resize() writes the correct 6-byte ConPTY signal packet.
    #[test]
    fn resize_writes_correct_signal_packet() {
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::namedpipeapi::CreatePipe;

        let mut read_end: HANDLE = std::ptr::null_mut();
        let mut signal_write: HANDLE = std::ptr::null_mut();
        let ok = unsafe { CreatePipe(&mut read_end, &mut signal_write, std::ptr::null_mut(), 0) };
        assert_ne!(ok, 0, "CreatePipe failed: {}", io::Error::last_os_error());

        let pty = unsafe {
            RawHandlesMasterPty::from_raw_handles(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                signal_write,
                std::ptr::null_mut(),
                PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            )
        };
        // from_raw_handles duplicated signal_write, so the original is still ours.
        unsafe {
            CloseHandle(signal_write);
        }

        let new_size = PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        };
        pty.resize(new_size).expect("resize should succeed");

        let mut buf = [0u8; 6];
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            winapi::um::fileapi::ReadFile(
                read_end,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0);
        assert_eq!(bytes_read, 6);

        let signal_id = u16::from_le_bytes([buf[0], buf[1]]);
        let cols = u16::from_le_bytes([buf[2], buf[3]]);
        let rows = u16::from_le_bytes([buf[4], buf[5]]);
        assert_eq!(signal_id, PTY_SIGNAL_RESIZE_WINDOW);
        assert_eq!(cols, 120);
        assert_eq!(rows, 40);

        let cached = pty.get_size().expect("get_size");
        assert_eq!(cached.rows, 40);
        assert_eq!(cached.cols, 120);

        unsafe {
            CloseHandle(read_end);
        }
    }

    #[test]
    fn resize_fails_gracefully_without_signal_pipe() {
        let pty = unsafe {
            RawHandlesMasterPty::from_raw_handles(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                PtySize::default(),
            )
        };
        let result = pty.resize(PtySize {
            rows: 50,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        });
        assert!(result.is_err());
    }

    /// P1-1: non-null `write` handle must yield a working writer.
    /// Before the fix, `let _ = write_owned;` dropped the OwnedHandle
    /// early, closing the handle; DuplicateHandle then failed.
    #[test]
    fn from_raw_handles_nonnull_write_yields_writer() {
        use std::io::Write;
        use winapi::um::fileapi::ReadFile;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::namedpipeapi::CreatePipe;

        let mut read_end: HANDLE = std::ptr::null_mut();
        let mut write_end: HANDLE = std::ptr::null_mut();
        let ok = unsafe { CreatePipe(&mut read_end, &mut write_end, std::ptr::null_mut(), 0) };
        assert_ne!(ok, 0);

        let pty = unsafe {
            RawHandlesMasterPty::from_raw_handles(
                std::ptr::null_mut(),
                write_end,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                PtySize::default(),
            )
        };

        let mut writer = pty
            .take_writer()
            .expect("take_writer must succeed when write handle was non-null");

        let payload: [u8; 1] = [0x5a];
        let written = writer
            .write(&payload)
            .expect("WriteFile through duplicated write handle should succeed");
        assert_eq!(written, 1);

        let mut buf = [0u8; 1];
        let mut bytes_read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                read_end,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0);
        assert_eq!(bytes_read, 1);
        assert_eq!(buf[0], 0x5a);

        unsafe {
            CloseHandle(read_end);
        }
    }
}
