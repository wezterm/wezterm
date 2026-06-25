// Vtable field names are mandated by the COM ABI / MIDL convention.
#![allow(non_snake_case)]

use std::convert::TryFrom;
use std::ffi::c_void;

use winapi::shared::ntdef::HANDLE;
use winapi::shared::winerror::{E_FAIL, S_OK};

use super::super::types::{bstr_to_string, TerminalStartupInfo};
use super::{TerminalStartupInfoOwned, HANDOFF_CALLBACK};

// winerror.h: `E_NOT_VALID_STATE = 0x8000FFFF`.
const E_NOT_VALID_STATE: i32 = 0x8000FFFFu32 as i32;

pub(super) unsafe extern "system" fn establish_pty_handoff(
    _this: *mut c_void,
    in_handle: *mut HANDLE,
    out_handle: *mut HANDLE,
    signal: HANDLE,
    reference: HANDLE,
    server: HANDLE,
    client: HANDLE,
    startup_info: *const TerminalStartupInfo,
) -> i32 {
    let callback = match HANDOFF_CALLBACK.get() {
        Some(cb) => cb,
        None => {
            log::error!("wezterm-termhost: EstablishPtyHandoff called but no callback registered");
            return E_NOT_VALID_STATE;
        }
    };

    let startup_owned = if startup_info.is_null() {
        TerminalStartupInfoOwned::default()
    } else {
        let s = &*startup_info;
        TerminalStartupInfoOwned {
            title: bstr_to_string(s.pszTitle),
            icon_path: bstr_to_string(s.pszIconPath),
            icon_index: s.iconIndex,
            show_window: s.wShowWindow,
            // Narrow u32 → u16; bogus out-of-range conhost values map to 0.
            width: u16::try_from(s.dwXSize).unwrap_or(0),
            height: u16::try_from(s.dwYSize).unwrap_or(0),
        }
    };

    log::info!(
        "wezterm-termhost: EstablishPtyHandoff received (title={:?}, in_out={:p}, out_out={:p}, \
         signal={:p}, reference={:p}, server={:p}, client={:p})",
        startup_owned.title,
        in_handle,
        out_handle,
        signal,
        reference,
        server,
        client,
    );

    match callback(
        in_handle,
        out_handle,
        signal,
        reference,
        server,
        client,
        startup_owned,
    ) {
        Ok(()) => S_OK,
        Err(e) => {
            log::error!("wezterm-termhost: handoff callback failed: {e:#}");
            E_FAIL
        }
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::super::{set_callback, HandoffCallback};
    use super::*;
    use crate::termhost::handoff::instance::VTABLE;
    use crate::termhost::handoff::take_singleton;
    use crate::termhost::raw_pty::create_anon_pipe;
    use std::os::windows::io::AsRawHandle;
    use std::sync::{Arc, Mutex};
    use winapi::shared::winerror::S_OK;
    use winapi::um::handleapi::CloseHandle;

    /// P1-2: the callback writes ConPTY-side handle values through the
    /// `*mut HANDLE` out-params; the caller's slots must contain those
    /// exact values after `establish_pty_handoff` returns `S_OK`.
    #[test]
    fn out_pipe_params_are_populated_by_callee() {
        let written: Arc<Mutex<(usize, usize)>> = Arc::new(Mutex::new((0, 0)));
        let captured = written.clone();

        let callback: HandoffCallback = Box::new(move |in_out, out_out, _, _, _, _, _| {
            let (their_read, _our_write) =
                create_anon_pipe(true, false).expect("create_anon_pipe for in");
            let (their_write, _our_read) =
                create_anon_pipe(true, true).expect("create_anon_pipe for out");

            let in_raw = their_read.as_raw_handle() as HANDLE;
            let out_raw = their_write.as_raw_handle() as HANDLE;

            // Forget the ConPTY-side OwnedHandles: their handles are now
            // owned by the caller's out-param slots.
            std::mem::forget(their_read);
            std::mem::forget(their_write);

            unsafe {
                if !in_out.is_null() {
                    *in_out = in_raw;
                }
                if !out_out.is_null() {
                    *out_out = out_raw;
                }
            }

            *captured.lock().unwrap() = (in_raw as usize, out_raw as usize);
            Ok(())
        });

        if set_callback(callback).is_err() {
            eprintln!(
                "out_pipe_params_are_populated_by_callee: \
                 HANDOFF_CALLBACK already set; skipping (OnceLock)."
            );
            return;
        }

        let mut in_handle: HANDLE = std::ptr::null_mut();
        let mut out_handle: HANDLE = std::ptr::null_mut();

        let this = take_singleton();
        let hr = unsafe {
            (VTABLE.EstablishPtyHandoff)(
                this,
                &mut in_handle,
                &mut out_handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert_eq!(hr, S_OK);

        let (expected_in, expected_out) = *written.lock().unwrap();
        assert_eq!(in_handle as usize, expected_in);
        assert_eq!(out_handle as usize, expected_out);
        assert!(!in_handle.is_null());
        assert!(!out_handle.is_null());

        unsafe {
            CloseHandle(in_handle);
            CloseHandle(out_handle);
        }
    }
}
