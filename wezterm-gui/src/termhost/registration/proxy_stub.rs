//! Proxy/stub DLL (`OpenConsoleProxy.dll`) registration for cross-process
//! COM marshalling of the handoff interfaces.

use std::path::{Path, PathBuf};
use winreg::enums::*;
use winreg::RegKey;

use super::clsid_registry_path;

// Must match the PROXY_CLSID_IS baked into OpenConsoleProxy.dll at compile
// time (microsoft/terminal Host.Proxy.vcxproj). The Stable channel CLSID
// is safe to share with WT MSIX because packaged COM and classic HKCU COM
// resolve through independent subsystems.
pub const WEZTERM_PROXY_STUB_CLSID: &str = "{3171DE52-6EFA-4AEF-8A9F-D02BD67E7A4F}";

/// Interface IIDs that the termhost handoff needs marshalled
/// (`microsoft/terminal/src/host/proxy/*.idl`):
/// - `IConsoleHandoff` — `{E686C757-...}` (first hop: conhost → OpenConsole)
/// - `ITerminalHandoff3` — `{6F23DA90-...}` (second hop: OpenConsole → terminal)
/// - `IDefaultTerminalMarker` — `{746E6BC0-...}` (QI target from conhost)
pub const TERMHOST_HANDOFF_IIDS: &[&str] = &[
    "{E686C757-9A35-4A1C-B3CE-0BCC8B5C69F4}",
    "{6F23DA90-15C5-4203-9DB0-64E73F1B1B00}",
    "{746E6BC0-AB05-4E38-AB14-71E86763141F}",
];

pub fn resolve_proxy_stub_dll_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = exe_dir.join("OpenConsoleProxy.dll");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Register the proxy/stub DLL per-user (HKCU ONLY — never HKLM).
///
/// We always register our own proxy stub, even when Windows Terminal is
/// installed via MSIX.  Classic HKCU COM and MSIX packaged-COM resolve
/// through independent subsystems, so our HKCU entries do not shadow or
/// interfere with WT's packaged registrations.  Relying solely on MSIX
/// packaged-COM is unreliable on some Windows 10 builds where the OS
/// fails to resolve packaged-COM classes for LocalServer32 activation.
///
/// Skips any entry that already has a non-empty value, so we never
/// clobber existing registrations.
pub fn register_proxy_stub_per_user(dll_path: &Path) -> anyhow::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let dll_str = dll_path.to_string_lossy().to_string();

    for iid in TERMHOST_HANDOFF_IIDS {
        let ps_path = format!("Software\\Classes\\Interface\\{}\\ProxyStubClsid32", iid);
        match hkcu.open_subkey_with_flags(&ps_path, KEY_READ) {
            Ok(sub) => {
                if let Ok(existing) = sub.get_value::<String, _>("") {
                    if !existing.trim().is_empty() {
                        log::info!(
                            "wezterm-termhost: Interface\\{}\\ProxyStubClsid32 \
                             already set to {}; skipping",
                            iid,
                            existing
                        );
                        continue;
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(anyhow::anyhow!("opening HKCU\\{}: {}", ps_path, e));
            }
        }

        let iid_path = format!("Software\\Classes\\Interface\\{}", iid);
        let (key, _) = hkcu
            .create_subkey(&iid_path)
            .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", iid_path, e))?;
        key.set_value("", &"WezTerm TermHost Handoff Interface")
            .map_err(|e| anyhow::anyhow!("writing Interface default value: {}", e))?;
        let (ps, _) = key
            .create_subkey("ProxyStubClsid32")
            .map_err(|e| anyhow::anyhow!("creating ProxyStubClsid32: {}", e))?;
        ps.set_value("", &WEZTERM_PROXY_STUB_CLSID)
            .map_err(|e| anyhow::anyhow!("writing ProxyStubClsid32 value: {}", e))?;
        log::info!(
            "wezterm-termhost: registered Interface\\{} -> {}",
            iid,
            WEZTERM_PROXY_STUB_CLSID
        );
    }

    let clsid_path = clsid_registry_path(WEZTERM_PROXY_STUB_CLSID);
    let (key, _) = hkcu
        .create_subkey(&clsid_path)
        .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", clsid_path, e))?;
    key.set_value("", &"WezTerm TermHost Proxy Stub")
        .map_err(|e| anyhow::anyhow!("writing CLSID default value: {}", e))?;
    let (inproc, _) = key
        .create_subkey("InProcServer32")
        .map_err(|e| anyhow::anyhow!("creating InProcServer32: {}", e))?;
    inproc
        .set_value("", &dll_str)
        .map_err(|e| anyhow::anyhow!("writing InProcServer32 value: {}", e))?;
    inproc
        .set_value("ThreadingModel", &"Both")
        .map_err(|e| anyhow::anyhow!("writing ThreadingModel: {}", e))?;
    log::info!(
        "wezterm-termhost: registered CLSID\\{}\\InProcServer32 -> {} (ThreadingModel=Both)",
        WEZTERM_PROXY_STUB_CLSID,
        dll_str
    );

    Ok(())
}
