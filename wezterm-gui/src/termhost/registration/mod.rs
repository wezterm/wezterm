//! Read/write the termhost registry keys under
//! `HKCU\Console\%%Startup\DelegationConsole` / `DelegationTerminal`,
//! read by conhost at session startup
//! (`microsoft/terminal/src/propslib/DelegationConfig.cpp:233-286`).
//!
//! HKCU only, never HKLM: touching HKLM can clobber WT's MSIX-managed
//! registrations and break ALL console apps.

use std::path::PathBuf;
use winreg::enums::*;
use winreg::RegKey;

#[cfg(test)]
use winapi::shared::guiddef::GUID;

#[cfg(test)]
use super::com_interfaces::CLSID_WezTermTerminalHandoff;

#[cfg(test)]
use super::com_interfaces::guid_to_string;

mod detect;
mod fallback;
mod proxy_stub;

pub(crate) use detect::find_clsid_server_path;
pub use detect::{clsid_server_exists, is_wt_installed};
pub use fallback::{register_openconsole_fallback, resolve_bundled_openconsole_path};
pub use proxy_stub::{
    register_proxy_stub_per_user, resolve_proxy_stub_dll_path, TERMHOST_HANDOFF_IIDS,
    WEZTERM_PROXY_STUB_CLSID,
};

pub const WEZTERM_TERMHOST_TERMINAL_CLSID: &str = "{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}";

/// Microsoft's stable OpenConsole CLSID (`{2EACA947-...}`, WT Release —
/// see `microsoft/terminal/src/cascadia/CascadiaPackage/Package.appxmanifest`).
/// WezTerm bundles a compatible `OpenConsole.exe` in `assets/windows/conhost/`
/// (copied to `target/<profile>/` by `wezterm-gui/build.rs`);
/// [`register_openconsole_fallback`] registers it under this CLSID when
/// no other COM server is registered, so handoff works without WT.
///
/// If no COM server is registered for this CLSID AND no fallback is
/// registered, every console app launch crashes with
/// `STATUS_DLL_INIT_FAILED` (`0xc0000142`).
pub const WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID: &str = "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}";

pub(crate) const TERMHOST_LET_WINDOWS_DECIDE: &str = "{00000000-0000-0000-0000-000000000000}";

#[cfg(test)]
pub(crate) const TERMHOST_WINDOWS_CONSOLE_HOST: &str = "{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}";

const STARTUP_KEY_PATH: &str = "Console\\%%Startup";

#[derive(Debug, Clone)]
pub struct TermHostRegistration {
    pub delegation_console: String,
    pub delegation_terminal: String,
}

impl Default for TermHostRegistration {
    fn default() -> Self {
        Self {
            delegation_console: WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID.to_string(),
            delegation_terminal: WEZTERM_TERMHOST_TERMINAL_CLSID.to_string(),
        }
    }
}

impl TermHostRegistration {
    /// Build a "Let Windows decide" registration (clears us as default).
    pub fn let_windows_decide() -> Self {
        Self {
            delegation_console: TERMHOST_LET_WINDOWS_DECIDE.to_string(),
            delegation_terminal: TERMHOST_LET_WINDOWS_DECIDE.to_string(),
        }
    }
}

pub fn register_termhost() -> anyhow::Result<()> {
    register_termhost_with(TermHostRegistration::default())
}

pub fn register_termhost_with(reg: TermHostRegistration) -> anyhow::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (startup, _) = hkcu
        .create_subkey(STARTUP_KEY_PATH)
        .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", STARTUP_KEY_PATH, e))?;
    startup
        .set_value("DelegationConsole", &reg.delegation_console)
        .map_err(|e| anyhow::anyhow!("writing DelegationConsole: {}", e))?;
    startup
        .set_value("DelegationTerminal", &reg.delegation_terminal)
        .map_err(|e| anyhow::anyhow!("writing DelegationTerminal: {}", e))?;
    log::info!(
        "wezterm-termhost: registered as default terminal (console={}, terminal={})",
        reg.delegation_console,
        reg.delegation_terminal
    );
    Ok(())
}

pub fn unregister_termhost() -> anyhow::Result<()> {
    register_termhost_with(TermHostRegistration::let_windows_decide())
}

/// Return the current `DelegationConsole` / `DelegationTerminal` values,
/// or `None` if not set.
pub fn current_registration() -> anyhow::Result<Option<TermHostRegistration>> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let startup = match hkcu.open_subkey_with_flags(STARTUP_KEY_PATH, KEY_READ) {
        Ok(k) => k,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "opening HKCU\\{} for read: {}",
                STARTUP_KEY_PATH,
                e
            ))
        }
    };
    let console: std::io::Result<String> = startup.get_value("DelegationConsole");
    let terminal: std::io::Result<String> = startup.get_value("DelegationTerminal");
    match (console, terminal) {
        (Ok(c), Ok(t)) => Ok(Some(TermHostRegistration {
            delegation_console: c,
            delegation_terminal: t,
        })),
        (Ok(_), Err(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!(
                "wezterm-termhost: DelegationConsole set but DelegationTerminal missing \
                 (partial state)"
            );
            Ok(None)
        }
        (Err(ref e), Ok(_)) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!(
                "wezterm-termhost: DelegationTerminal set but DelegationConsole missing \
                 (partial state)"
            );
            Ok(None)
        }
        (Ok(_), Err(e)) => Err(anyhow::anyhow!("reading termhost terminal key: {}", e)),
        (Err(e), _) => Err(anyhow::anyhow!("reading termhost keys: {}", e)),
    }
}

pub(crate) fn clsid_registry_path(clsid: &str) -> String {
    format!("Software\\Classes\\CLSID\\{}", clsid)
}

/// Read `LocalServer32` for a CLSID from a registry root (typically
/// `HKEY_CURRENT_USER` or `HKEY_LOCAL_MACHINE`), extract the exe path
/// (stripping quotes and args), and return it as a `PathBuf`. Returns
/// `None` if missing or empty.
pub fn read_local_server_exe(root: &RegKey, clsid: &str) -> Option<PathBuf> {
    let path = format!("Software\\Classes\\CLSID\\{}\\LocalServer32", clsid);
    let key = root.open_subkey_with_flags(&path, KEY_READ).ok()?;
    let value: String = key.get_value("").ok()?;
    let exe = extract_exe_token(&value);
    if exe.is_empty() {
        None
    } else {
        Some(PathBuf::from(exe))
    }
}

/// Extract the exe path token from a `LocalServer32` value:
/// `"C:\path\with spaces.exe" --args`, `C:\path\no-spaces.exe`, or
/// `C:\path\no-spaces.exe --args`.
pub fn extract_exe_token(value: &str) -> &str {
    let trimmed = value.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return &rest[..end];
        }
    }
    trimmed.split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
pub fn parse_guid(s: &str) -> Option<GUID> {
    let s = s.trim();
    let s = s.trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = s.splitn(5, '-').collect();
    if parts.len() != 5 {
        return None;
    }
    let data1 = u32::from_str_radix(parts[0], 16).ok()?;
    let data2 = u16::from_str_radix(parts[1], 16).ok()?;
    let data3 = u16::from_str_radix(parts[2], 16).ok()?;
    if parts[3].len() != 4 || parts[4].len() != 12 {
        return None;
    }
    let mut data4 = [0u8; 8];
    for (i, byte_str) in parts[3]
        .as_bytes()
        .chunks(2)
        .chain(parts[4].as_bytes().chunks(2))
        .enumerate()
    {
        let hex = std::str::from_utf8(byte_str).ok()?;
        data4[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wezterm_guid() {
        let g = parse_guid(WEZTERM_TERMHOST_TERMINAL_CLSID).expect("parse wezterm guid");
        let s = guid_to_string(&g);
        assert_eq!(
            s.to_uppercase(),
            WEZTERM_TERMHOST_TERMINAL_CLSID.to_uppercase()
        );
    }

    #[test]
    fn parse_microsoft_terminal_guid() {
        let g = parse_guid("{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}").expect("parse ms guid");
        assert_eq!(g.Data1, 0xE12CFF52);
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_guid("not a guid").is_none());
        assert!(parse_guid("{too-short}").is_none());
        assert!(parse_guid("{XXXXXXXX-0000-0000-0000-000000000000}").is_none());
    }

    #[test]
    fn roundtrip_all_known_clsids() {
        for s in [
            WEZTERM_TERMHOST_TERMINAL_CLSID,
            WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID,
            TERMHOST_LET_WINDOWS_DECIDE,
            TERMHOST_WINDOWS_CONSOLE_HOST,
        ] {
            let g = parse_guid(s).unwrap_or_else(|| panic!("parse {}", s));
            let back = guid_to_string(&g);
            assert_eq!(back.to_uppercase(), s.to_uppercase(), "roundtrip {}", s);
        }
    }

    #[test]
    fn const_guids_match_strings() {
        let wez = parse_guid(WEZTERM_TERMHOST_TERMINAL_CLSID).unwrap();
        assert!(wez.Data1 == CLSID_WezTermTerminalHandoff.Data1);
        assert!(wez.Data2 == CLSID_WezTermTerminalHandoff.Data2);
        assert!(wez.Data3 == CLSID_WezTermTerminalHandoff.Data3);
        assert!(wez.Data4 == CLSID_WezTermTerminalHandoff.Data4);
    }
}
