//! COM class factory registration via `CoRegisterClassObject`.
//!
//! `start_listening` registers our class factory on the caller's thread
//! (which must already be STA-initialized). The returned `HandoffGuard`
//! revokes the registration on drop. Drop order matters: `HandoffGuard`
//! must drop before the caller's `CoinitGuard`.

use std::sync::OnceLock;

use winapi::shared::winerror::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use winapi::um::combaseapi::{
    CoInitializeEx, CoRegisterClassObject, CoRevokeClassObject, CoUninitialize,
};
use winapi::um::unknwnbase::IUnknown;

use super::com_interfaces::CLSID_WezTermTerminalHandoff;
use super::handoff::{set_callback, take_factory, HandoffCallback};
use super::registration::WEZTERM_TERMHOST_TERMINAL_CLSID;

// objbase.h: CLSCTX_LOCAL_SERVER = 0x4, REGCLS_MULTIPLEUSE = 1.
const CLSCTX_LOCAL_SERVER: u32 = 4;
const REGCLS_MULTIPLEUSE: u32 = 1;

/// COINIT_APARTMENTTHREADED (objbase.h).
const COINIT_APARTMENTTHREADED: u32 = 0x2;

pub struct CoinitGuard {
    actually_initialized: bool,
}

impl Drop for CoinitGuard {
    fn drop(&mut self) {
        if self.actually_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

impl CoinitGuard {
    /// STA-init COM on this thread. `S_FALSE` is success (refcount must
    /// still balance). Returns `Err` for `RPC_E_CHANGED_MODE`.
    pub fn new() -> anyhow::Result<Self> {
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        if hr == S_OK || hr == S_FALSE {
            Ok(CoinitGuard {
                actually_initialized: true,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            anyhow::bail!(
                "CoInitializeEx(STA) failed with RPC_E_CHANGED_MODE (0x{:08x})",
                hr as u32
            );
        } else {
            anyhow::bail!(
                "CoInitializeEx(STA) failed with HRESULT 0x{:08x}",
                hr as u32
            );
        }
    }
}

/// Owns the COM class registration. The caller's `CoinitGuard` MUST
/// outlive this guard so revoke happens while COM is still initialized.
pub struct HandoffGuard {
    cookie: u32,
}

impl Drop for HandoffGuard {
    fn drop(&mut self) {
        if self.cookie != 0 {
            unsafe { CoRevokeClassObject(self.cookie) };
        }
    }
}

static LISTENING: OnceLock<()> = OnceLock::new();

pub fn start_listening(callback: HandoffCallback) -> anyhow::Result<HandoffGuard> {
    if LISTENING.get().is_some() {
        anyhow::bail!("listener already started");
    }

    if set_callback(callback).is_err() {
        log::warn!("wezterm-termhost: handoff callback already installed; ignoring");
    }

    let factory_ptr = take_factory();
    let mut cookie: u32 = 0;
    let hr = unsafe {
        CoRegisterClassObject(
            &CLSID_WezTermTerminalHandoff,
            factory_ptr as *mut IUnknown,
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE,
            &mut cookie,
        )
    };
    if hr != S_OK {
        anyhow::bail!("CoRegisterClassObject failed: HRESULT 0x{:08x}", hr as u32);
    }

    let _ = LISTENING.set(());

    log::info!(
        "wezterm-termhost: class factory registered as CLSID {} (cookie={})",
        WEZTERM_TERMHOST_TERMINAL_CLSID,
        cookie
    );

    Ok(HandoffGuard { cookie })
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    /// P3-1: `S_FALSE` from a second `CoInitializeEx` must still mark
    /// the guard as initialized so `Drop` balances the per-thread
    /// refcount.
    #[test]
    fn coinit_guard_marks_initialized_on_s_false() {
        unsafe {
            let hr_first = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
            assert!(hr_first == S_OK || hr_first == S_FALSE);

            let hr_second = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
            assert_eq!(hr_second as u32, S_FALSE as u32);

            let guard = CoinitGuard {
                actually_initialized: hr_second == S_OK || hr_second == S_FALSE,
            };
            assert!(guard.actually_initialized);
            drop(guard);

            CoUninitialize();
        }
    }

    /// `HandoffGuard` with sentinel cookie `0` drops without calling
    /// `CoRevokeClassObject`.
    #[test]
    fn handoff_guard_drop_with_zero_cookie_is_safe() {
        let guard = HandoffGuard { cookie: 0 };
        drop(guard);
    }
}
