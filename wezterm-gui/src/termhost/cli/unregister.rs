use crate::termhost::{TERMHOST_HANDOFF_IIDS, WEZTERM_PROXY_STUB_CLSID};

pub struct UnregisterCommand {}

impl UnregisterCommand {
    pub fn run() -> anyhow::Result<()> {
        crate::termhost::unregister_termhost()?;
        println!("WezTerm is no longer registered as the Windows default terminal.");

        unregister_local_server_for_unpackaged()?;
        println!(
            "Removed HKCU\\Software\\Classes\\CLSID\\{}\\LocalServer32.",
            crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID
        );

        unregister_proxy_stub_per_user();
        unregister_openconsole_fallback();

        Ok(())
    }
}

pub(crate) fn unregister_local_server_for_unpackaged() -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let clsid = crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID;
    let key_path = crate::termhost::registration::clsid_registry_path(clsid);

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&key_path, KEY_WRITE) {
        let _ = clsid_key.delete_subkey("LocalServer32");
    }
    let _ = hkcu.delete_subkey(&key_path);
    Ok(())
}

fn unregister_proxy_stub_per_user() {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    for iid in TERMHOST_HANDOFF_IIDS {
        let iid_path = format!("Software\\Classes\\Interface\\{}", iid);
        if let Ok(iid_key) = hkcu.open_subkey_with_flags(&iid_path, KEY_WRITE) {
            let _ = iid_key.delete_subkey("ProxyStubClsid32");
        }
        let _ = hkcu.delete_subkey(&iid_path);
    }

    let clsid_path = format!("Software\\Classes\\CLSID\\{}", WEZTERM_PROXY_STUB_CLSID);
    if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&clsid_path, KEY_WRITE) {
        let _ = clsid_key.delete_subkey("InProcServer32");
    }
    let _ = hkcu.delete_subkey(&clsid_path);

    println!("Removed per-user proxy/stub registry entries (if any).");
}

fn unregister_openconsole_fallback() {
    use winreg::enums::*;
    use winreg::RegKey;

    let clsid = crate::termhost::WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID;
    let key_path = crate::termhost::registration::clsid_registry_path(clsid);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let owned_by_wezterm = match hkcu.open_subkey_with_flags(&key_path, KEY_READ) {
        Ok(clsid_key) => clsid_key.get_value::<u32, _>("WezTermOwned").unwrap_or(0) == 1,
        Err(_) => false,
    };

    if owned_by_wezterm {
        if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&key_path, KEY_WRITE) {
            let _ = clsid_key.delete_subkey("LocalServer32");
        }
        let _ = hkcu.delete_subkey(&key_path);
        println!(
            "Removed HKCU\\Software\\Classes\\CLSID\\{}\\LocalServer32 (bundled OpenConsole fallback).",
            clsid
        );
    }
}
