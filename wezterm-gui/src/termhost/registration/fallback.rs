//! Bundled `OpenConsole.exe` fallback registration.

use std::path::{Path, PathBuf};
use winreg::enums::*;
use winreg::RegKey;

use super::{clsid_registry_path, find_clsid_server_path, WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID};

/// Resolve the path to the bundled OpenConsole.exe. Looks for:
/// (1) `<exe_dir>/OpenConsole.exe` — dev-build AND bundled-install layout
///     (`wezterm-gui/build.rs` copies it next to `wezterm-gui.exe`).
/// (2) Walking up from `exe_dir` to find `target/<debug|release>/OpenConsole.exe`
///     — defensive fallback for unusual install layouts.
pub fn resolve_bundled_openconsole_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    let candidate = exe_dir.join("OpenConsole.exe");
    if candidate.exists() {
        return Some(candidate);
    }

    let mut dir: &Path = exe_dir.as_path();
    while let Some(parent) = dir.parent() {
        let target_dir = parent.join("target");
        if target_dir.is_dir() {
            for profile in ["debug", "release"] {
                let candidate = target_dir.join(profile).join("OpenConsole.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        dir = parent;
    }

    None
}

/// Register the bundled `OpenConsole.exe` under
/// [`WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID`] in HKCU, IF no COM server
/// is already registered (HKCU or HKLM) with a valid exe path for that
/// CLSID. Safety net preventing `0xc0000142` (`STATUS_DLL_INIT_FAILED`)
/// on machines without WT Release.
pub fn register_openconsole_fallback() -> anyhow::Result<()> {
    let bundled = match resolve_bundled_openconsole_path() {
        Some(p) => p,
        None => {
            log::warn!(
                "wezterm-termhost: bundled OpenConsole.exe not found; \
                 skipping fallback registration"
            );
            return Ok(());
        }
    };

    let clsid = WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID;
    if let Some(existing) = find_clsid_server_path(clsid) {
        log::info!(
            "wezterm-termhost: OpenConsole already registered at {}; skipping fallback",
            existing.display()
        );
        return Ok(());
    }

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key_path = clsid_registry_path(clsid);
    let (key, _) = hkcu
        .create_subkey(&key_path)
        .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", key_path, e))?;
    key.set_value("", &"WezTerm-bundled OpenConsole (Microsoft MIT-licensed)")
        .map_err(|e| anyhow::anyhow!("writing CLSID default value: {}", e))?;
    key.set_value("WezTermOwned", &1u32)
        .map_err(|e| anyhow::anyhow!("writing WezTermOwned marker: {}", e))?;
    let (local_server, _) = key
        .create_subkey("LocalServer32")
        .map_err(|e| anyhow::anyhow!("creating LocalServer32: {}", e))?;
    let value = format!("\"{}\"", bundled.display());
    local_server
        .set_value("", &value)
        .map_err(|e| anyhow::anyhow!("writing LocalServer32 value: {}", e))?;
    log::info!(
        "wezterm-termhost: registered bundled OpenConsole.exe at {} \
         (HKCU\\Software\\Classes\\CLSID\\{}\\LocalServer32)",
        bundled.display(),
        clsid
    );
    Ok(())
}
