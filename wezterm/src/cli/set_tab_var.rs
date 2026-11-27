use clap::Parser;
use mux::pane::PaneId;
use mux::tab::TabId;
use std::collections::HashMap;
use wezterm_client::client::Client;

#[derive(Debug, Parser, Clone)]
pub struct SetTabVar {
    /// Specify the target tab by its id
    #[arg(long, conflicts_with_all=&["pane_id"])]
    tab_id: Option<TabId>,
    /// Specify the current pane.
    /// The default is to use the current pane based on the
    /// environment variable WEZTERM_PANE.
    ///
    /// The pane is used to figure out which tab to set the variable on.
    #[arg(long)]
    pane_id: Option<PaneId>,

    /// Remove the variable instead of setting it
    #[arg(long)]
    unset: bool,

    /// The name of the variable
    name: String,

    /// The value to set the variable to
    #[arg(required_unless_present = "unset", conflicts_with = "unset")]
    value: Option<String>,
}

impl SetTabVar {
    pub async fn run(self, client: Client) -> anyhow::Result<()> {
        anyhow::ensure!(!self.name.is_empty(), "a tab variable needs a name");
        let panes = client.list_panes().await?;

        let mut pane_id_to_tab_id = HashMap::new();

        for tabroot in panes.tabs {
            let mut cursor = tabroot.into_tree().cursor();

            loop {
                if let Some(entry) = cursor.leaf_mut() {
                    pane_id_to_tab_id.insert(entry.pane_id, entry.tab_id);
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(_) => break,
                }
            }
        }

        let tab_id = if let Some(tab_id) = self.tab_id {
            tab_id
        } else {
            // Find the current tab from the pane id
            let pane_id = client.resolve_pane_id(self.pane_id).await?;
            pane_id_to_tab_id
                .get(&pane_id)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("unable to resolve current tab"))?
        };

        client
            .set_tab_var(codec::TabVarChanged {
                tab_id,
                name: self.name,
                value: self.value,
            })
            .await?;
        Ok(())
    }
}
