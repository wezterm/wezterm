use clap::Parser;

use super::{find_host_by_clsids, KNOWN_HOSTS};

#[derive(Debug, Parser, Clone)]
pub struct ListCommand {}

impl ListCommand {
    pub fn run() -> anyhow::Result<()> {
        print_current_default()?;
        print_known_hosts_table()?;
        print_msix_hosts();
        Ok(())
    }
}

fn print_current_default() -> anyhow::Result<()> {
    let current = crate::termhost::current_registration()?;
    match current {
        None => println!("Current default terminal: (not set; Windows will decide)\n"),
        Some(reg) => {
            let name = find_host_by_clsids(&reg.delegation_console, &reg.delegation_terminal)
                .map(|h| h.display_name)
                .unwrap_or("unknown");
            println!("Current default terminal: {}\n", name);
            println!(
                "  DelegationConsole : {}\n  DelegationTerminal: {}\n",
                reg.delegation_console, reg.delegation_terminal
            );
        }
    }
    Ok(())
}

fn print_known_hosts_table() -> anyhow::Result<()> {
    println!("Known terminal hosts:");
    println!(
        "  {:<12} {:<32} {:<11} {:<7}",
        "ID", "NAME", "INSTALLED", "DEFAULT"
    );
    let current = crate::termhost::current_registration()?;
    for h in KNOWN_HOSTS {
        let installed = if h.id == "conhost" {
            true
        } else {
            is_host_installed(h.console_clsid)
        };
        let is_default = current
            .as_ref()
            .map(|c| {
                h.console_clsid.eq_ignore_ascii_case(&c.delegation_console)
                    && (h.terminal_clsid.is_empty()
                        || h.terminal_clsid
                            .eq_ignore_ascii_case(&c.delegation_terminal))
            })
            .unwrap_or(false);
        println!(
            "  {:<12} {:<32} {:<11} {:<7}",
            h.id,
            h.display_name,
            if installed { "yes" } else { "?" },
            if is_default { "*" } else { "" }
        );
    }
    println!();
    Ok(())
}

fn is_host_installed(console_clsid: &str) -> bool {
    crate::termhost::registration::clsid_server_exists(console_clsid)
}

fn print_msix_hosts() {
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-AppxPackage | Where-Object { $_.Name -match 'Terminal|wezterm' } | ForEach-Object { \"$($_.Name)|$($_.Version)\" }",
        ])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return;
    }
    println!("Installed MSIX terminal packages:");
    for line in lines {
        let mut parts = line.splitn(2, '|');
        let name = parts.next().unwrap_or(line);
        let version = parts.next().unwrap_or("?");
        println!("  {} {}", name, version);
    }
}
