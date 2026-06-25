use wezterm_gui_subcommands::TerminalHostRegisterArgs;

pub struct RegisterCommand {}

impl RegisterCommand {
    pub fn run(args: &TerminalHostRegisterArgs) -> anyhow::Result<()> {
        crate::termhost::register_termhost()?;
        println!(
            "WezTerm is now registered as the Windows default terminal.\n\
             \n\
             Terminal CLSID : {}\n\
             Console CLSID  : {} (Microsoft OpenConsole.exe; bundled copy registered as fallback)\n\
             Registry key   : HKCU\\Console\\%%Startup\\DelegationConsole, DelegationTerminal",
            crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID,
            crate::termhost::WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID
        );

        if !args.no_local_server {
            register_local_server_for_unpackaged()?;
            println!(
                "\nRegistered HKCU\\Software\\Classes\\CLSID\\{}\\LocalServer32 -> wezterm-gui.exe.",
                crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID
            );
        }

        if let Err(e) = crate::termhost::register_openconsole_fallback() {
            eprintln!("warning: OpenConsole fallback registration skipped: {}", e);
        } else if crate::termhost::resolve_bundled_openconsole_path().is_none() {
            eprintln!(
                "warning: bundled OpenConsole.exe not found. \
                 Console launches will crash with 0xc0000142 until \
                 OpenConsole.exe is available next to wezterm-gui.exe."
            );
        }

        if !args.no_proxy_stub {
            if let Some(dll) = crate::termhost::resolve_proxy_stub_dll_path() {
                if let Err(e) = crate::termhost::register_proxy_stub_per_user(&dll) {
                    eprintln!("warning: proxy/stub registration skipped: {}", e);
                }
            } else if !crate::termhost::is_wt_installed() {
                eprintln!(
                    "note: proxy/stub DLL not found next to wezterm-gui.exe; \
                     skipping per-user registration. The DLL is bundled in \
                     assets/windows/conhost/ and copied by the build."
                );
            }
        }

        Ok(())
    }
}

pub(crate) fn register_local_server_for_unpackaged() -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let exe_path = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("determining wezterm-gui.exe path: {}", e))?;
    let exe_str = exe_path.to_string_lossy().to_string();

    let clsid = crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID;
    let key_path = crate::termhost::registration::clsid_registry_path(clsid);

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(&key_path)
        .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", key_path, e))?;
    key.set_value("", &"WezTerm Default Terminal Handoff")
        .map_err(|e| anyhow::anyhow!("writing default value: {}", e))?;

    let (local_server, _) = key
        .create_subkey("LocalServer32")
        .map_err(|e| anyhow::anyhow!("creating LocalServer32: {}", e))?;
    let cmd_line = format!("\"{}\"", exe_str);
    local_server
        .set_value("", &cmd_line)
        .map_err(|e| anyhow::anyhow!("writing LocalServer32 value: {}", e))?;

    Ok(())
}
