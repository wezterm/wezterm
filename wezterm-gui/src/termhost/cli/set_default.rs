use crate::termhost::{register_termhost_with, TermHostRegistration};
use wezterm_gui_subcommands::TerminalHostSetDefaultArgs;

use super::{find_host, KNOWN_HOSTS};

pub struct SetDefaultCommand {}

impl SetDefaultCommand {
    pub fn run(args: &TerminalHostSetDefaultArgs) -> anyhow::Result<()> {
        let (console, terminal, display_name) = resolve_host(&args.host)?;
        let reg = TermHostRegistration {
            delegation_console: console.clone(),
            delegation_terminal: terminal.clone(),
        };
        register_termhost_with(reg)?;
        println!("Default terminal set to {}.", display_name);
        println!(
            "  DelegationConsole : {}\n  DelegationTerminal: {}",
            console, terminal
        );
        Ok(())
    }
}

fn resolve_host(arg: &str) -> anyhow::Result<(String, String, String)> {
    if let Some(h) = find_host(arg) {
        return Ok((
            h.console_clsid.to_string(),
            h.terminal_clsid.to_string(),
            h.display_name.to_string(),
        ));
    }
    if looks_like_clsid(arg) {
        let available: Vec<&str> = KNOWN_HOSTS.iter().map(|h| h.id).collect();
        anyhow::bail!(
            "raw CLSIDs are not supported; use a known host id: {}",
            available.join(", ")
        );
    }
    let available: Vec<&str> = KNOWN_HOSTS.iter().map(|h| h.id).collect();
    anyhow::bail!(
        "unknown host id `{}`; expected one of: {}",
        arg,
        available.join(", ")
    )
}

fn looks_like_clsid(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('{') && s.ends_with('}') && s.len() == 38
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_clsid_rejects_short_strings() {
        assert!(!looks_like_clsid(""));
        assert!(!looks_like_clsid("wezterm"));
        assert!(!looks_like_clsid("{abc}"));
    }

    #[test]
    fn looks_like_clsid_accepts_well_formed() {
        assert!(looks_like_clsid("{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}"));
    }

    #[test]
    fn resolve_host_finds_known_id() {
        let (console, terminal, name) = resolve_host("wezterm").unwrap();
        assert_eq!(console, "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}");
        assert_eq!(terminal, "{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}");
        assert_eq!(name, "WezTerm");
    }

    #[test]
    fn resolve_host_rejects_unknown_id() {
        assert!(resolve_host("nope").is_err());
    }
}
