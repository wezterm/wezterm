use crate::termhost::unregister_termhost;

pub struct ResetCommand {}

impl ResetCommand {
    pub fn run() -> anyhow::Result<()> {
        unregister_termhost()?;
        println!(
            "Default terminal reset to \"Let Windows decide\".\n\
             DelegationConsole and DelegationTerminal set to \
             {{00000000-0000-0000-0000-000000000000}}."
        );
        Ok(())
    }
}
