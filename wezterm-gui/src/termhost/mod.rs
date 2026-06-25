//! Windows Default Terminal handoff via `ITerminalHandoff3`.
//!
//! When conhost delegates a PTY to us we open a new tab attached to it.

#[cfg(windows)]
pub mod com_interfaces;
#[cfg(windows)]
pub mod handoff;
#[cfg(windows)]
pub mod raw_pty;
#[cfg(windows)]
pub mod registration;
#[cfg(windows)]
pub mod server;
#[cfg(windows)]
pub mod types;

#[cfg(windows)]
mod integration;

#[cfg(windows)]
pub(crate) mod cli;

#[cfg(windows)]
pub use handoff::{HandoffCallback, TerminalStartupInfoOwned};
#[cfg(windows)]
pub use raw_pty::{create_anon_pipe, RawHandlesMasterPty, TermHostChild};
#[cfg(windows)]
pub use registration::{
    current_registration, is_wt_installed, register_openconsole_fallback,
    register_proxy_stub_per_user, register_termhost, register_termhost_with,
    resolve_bundled_openconsole_path, resolve_proxy_stub_dll_path, unregister_termhost,
    TermHostRegistration, TERMHOST_HANDOFF_IIDS, WEZTERM_PROXY_STUB_CLSID,
    WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID, WEZTERM_TERMHOST_TERMINAL_CLSID,
};
#[cfg(windows)]
pub use server::{start_listening, CoinitGuard, HandoffGuard};

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use winapi::shared::ntdef::HANDLE;

static LISTENER_STARTED: OnceLock<()> = OnceLock::new();

static SCM_LAUNCHED: OnceLock<bool> = OnceLock::new();

static HANDOFF_RECEIVED: AtomicBool = AtomicBool::new(false);

pub fn set_scm_launched(v: bool) {
    let _ = SCM_LAUNCHED.set(v);
}

pub fn scm_launched() -> bool {
    *SCM_LAUNCHED.get().unwrap_or(&false)
}

/// Holds the termhost COM registration. Drop order matters: `handoff`
/// must drop before `coinit` so `CoRevokeClassObject` runs while COM is
/// still initialized on this thread. Rust drops fields in declaration
/// order, so `handoff` is declared first.
#[allow(dead_code)]
pub struct TermHostState {
    handoff: Option<HandoffGuard>,
    coinit: CoinitGuard,
}

pub fn install() -> Option<TermHostState> {
    let coinit = match CoinitGuard::new() {
        Ok(g) => g,
        Err(e) => {
            log::error!("CoInitializeEx(STA) on main thread failed: {e:#}");
            return None;
        }
    };
    let handoff = match try_start_listener() {
        Ok(g) => g,
        Err(e) => {
            log::error!("termhost listener failed to start: {e:#}");
            return None;
        }
    };
    Some(TermHostState { handoff, coinit })
}

/// Detect SCM launch (`-Embedding` / `/Embedding`) and strip the flag
/// before clap parsing. Mirrors WindowEmperor.cpp:591.
pub fn preprocess_argv() -> (Vec<OsString>, bool) {
    filter_embedding_flags(std::env::args_os())
}

fn filter_embedding_flags(argv: impl Iterator<Item = OsString>) -> (Vec<OsString>, bool) {
    let mut scm_launched = false;
    let filtered: Vec<OsString> = argv
        .filter(|a| {
            let b = a.as_encoded_bytes();
            if b == b"-Embedding" || b == b"/Embedding" {
                scm_launched = true;
                false
            } else {
                true
            }
        })
        .collect();
    (filtered, scm_launched)
}

/// Hold an Activity guard for 5s to suppress `MuxNotification::Empty`
/// termination while we wait for the COM handoff; spawn the
/// default-profile tab as fallback if none arrives.
pub fn await_handoff() {
    promise::spawn::spawn(async move {
        let _activity = mux::activity::Activity::new();

        smol::Timer::after(std::time::Duration::from_secs(5)).await;

        if !HANDOFF_RECEIVED.load(Ordering::SeqCst) {
            if let Err(e) =
                crate::spawn_tab_in_domain_if_mux_is_empty(None, false, None, None).await
            {
                log::error!("wezterm-termhost: fallback spawn failed: {e:#}");
            }
        }
    })
    .detach();
}

pub(crate) fn try_start_listener() -> anyhow::Result<Option<HandoffGuard>> {
    if LISTENER_STARTED.get().is_some() {
        return Ok(None);
    }

    let callback: HandoffCallback = Box::new(integration::handle_handoff);
    let guard = start_listening(callback)?;
    let _ = LISTENER_STARTED.set(());
    log::info!("wezterm-gui: termhost listener started");
    Ok(Some(guard))
}

fn pid_of(handle: HANDLE) -> Option<u32> {
    if handle.is_null() {
        return None;
    }
    unsafe {
        use winapi::um::processthreadsapi::GetProcessId;
        let pid = GetProcessId(handle);
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn filter_embedding_flags_strips_dash_embedding() {
        let argv = vec![
            os("wezterm-gui"),
            os("-Embedding"),
            os("--config-file"),
            os("foo.lua"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0], os("wezterm-gui"));
        assert_eq!(filtered[1], os("--config-file"));
        assert_eq!(filtered[2], os("foo.lua"));
        assert!(scm_launched);
    }

    #[test]
    fn filter_embedding_flags_strips_slash_embedding() {
        let argv = vec![os("wezterm-gui"), os("/Embedding")].into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], os("wezterm-gui"));
        assert!(scm_launched);
    }

    #[test]
    fn filter_embedding_flags_preserves_normal_args() {
        let argv = vec![
            os("wezterm-gui"),
            os("start"),
            os("--class"),
            os("my-class"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 4);
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_rejects_near_misses() {
        let argv = vec![
            os("--embedding"),
            os("-embeddings"),
            os("-Embedding="),
            os("-embedding"),
            os("/embedding"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 5);
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_handles_empty_argv() {
        let argv = std::iter::empty::<OsString>();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert!(filtered.is_empty());
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_multiple_embedding_flags_all_stripped() {
        let argv = vec![
            os("-Embedding"),
            os("/Embedding"),
            os("-Embedding"),
            os("subcommand"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], os("subcommand"));
        assert!(scm_launched);
    }
}
