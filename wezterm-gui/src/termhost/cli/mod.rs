//! Windows default terminal host management.
//!
//! Mirrors the `wezterm cli` subcommand pattern: each action is a separate
//! struct in its own file, dispatched via [`run`] from a `match` on
//! [`wezterm_gui_subcommands::TerminalHostSub`].

use wezterm_gui_subcommands::{TerminalHostCommand, TerminalHostSub};

mod list;
mod register;
mod reset;
mod set_default;
mod unregister;

pub(crate) struct KnownHost {
    pub(crate) id: &'static str,
    pub(crate) display_name: &'static str,
    pub(crate) console_clsid: &'static str,
    pub(crate) terminal_clsid: &'static str,
}

pub(crate) const KNOWN_HOSTS: &[KnownHost] = &[
    KnownHost {
        id: "conhost",
        display_name: "Windows Console Host (classic)",
        console_clsid: "{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}",
        terminal_clsid: "{00000000-0000-0000-0000-000000000000}",
    },
    KnownHost {
        id: "wt-release",
        display_name: "Windows Terminal (Release)",
        console_clsid: "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}",
        terminal_clsid: "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}",
    },
    KnownHost {
        id: "wt-preview",
        display_name: "Windows Terminal (Preview)",
        console_clsid: "{06EC847C-C0A5-46B8-92CB-7C92F6E35CD5}",
        terminal_clsid: "{86633F1F-6454-40EC-89CE-DA4EBA977EE2}",
    },
    KnownHost {
        id: "wt-canary",
        display_name: "Windows Terminal (Canary)",
        console_clsid: "{A854D02A-F2FE-44A5-BB24-D03F4CF830D4}",
        terminal_clsid: "{1706609C-A4CE-4C0D-B7D2-C19BF66398A5}",
    },
    KnownHost {
        id: "wt-dev",
        display_name: "Windows Terminal (Dev)",
        console_clsid: "{1F9F2BF5-5BC3-4F17-B0E6-912413F1F451}",
        terminal_clsid: "{051F34EE-C1FD-4B19-AF75-9BA54648434C}",
    },
    KnownHost {
        id: "wezterm",
        display_name: "WezTerm",
        console_clsid: "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}",
        terminal_clsid: "{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}",
    },
];

pub(crate) fn find_host(id: &str) -> Option<&'static KnownHost> {
    KNOWN_HOSTS.iter().find(|h| h.id.eq_ignore_ascii_case(id))
}

pub(crate) fn find_host_by_clsids(console: &str, terminal: &str) -> Option<&'static KnownHost> {
    KNOWN_HOSTS.iter().find(|h| {
        h.console_clsid.eq_ignore_ascii_case(console)
            && h.terminal_clsid.eq_ignore_ascii_case(terminal)
    })
}

pub fn run(cmd: TerminalHostCommand) -> anyhow::Result<()> {
    match cmd.sub {
        TerminalHostSub::List => list::ListCommand::run(),
        TerminalHostSub::Register(args) => register::RegisterCommand::run(&args),
        TerminalHostSub::Unregister => unregister::UnregisterCommand::run(),
        TerminalHostSub::SetDefault(args) => set_default::SetDefaultCommand::run(&args),
        TerminalHostSub::Reset => reset::ResetCommand::run(),
    }
}
