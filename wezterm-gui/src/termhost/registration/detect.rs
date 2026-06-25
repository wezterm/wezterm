//! Windows Terminal detection: checks whether WT-branded OpenConsole
//! CLSIDs are registered, and whether any registered COM `LocalServer32`
//! entry exists for a given CLSID.

use std::path::PathBuf;
use winreg::enums::*;
use winreg::RegKey;

use super::read_local_server_exe;

/// OpenConsole CLSIDs from the four WT channels
/// (`microsoft/terminal/src/cascadia/CascadiaPackage/Package-*.appxmanifest`).
fn wt_brand_openconsole_clsids() -> Vec<&'static str> {
    crate::termhost::cli::KNOWN_HOSTS
        .iter()
        .filter(|h| h.id.starts_with("wt-"))
        .map(|h| h.console_clsid)
        .collect()
}

/// Path of the registered COM `LocalServer32` exe for `clsid` under
/// HKCU or HKLM, if it exists AND points at an existing file.
pub(crate) fn find_clsid_server_path(clsid: &str) -> Option<PathBuf> {
    for root in [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ] {
        if let Some(path) = read_local_server_exe(&root, clsid) {
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

pub fn clsid_server_exists(clsid: &str) -> bool {
    find_clsid_server_path(clsid).is_some()
}

/// Returns `true` iff any Windows Terminal brand
/// (Release / Preview / Canary / Dev) is installed.
pub fn is_wt_installed() -> bool {
    wt_brand_openconsole_clsids()
        .iter()
        .any(|c| clsid_server_exists(c))
}
