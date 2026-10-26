use crate::cli::CliOutputFormatKind;
use clap::Parser;
use serde::Serializer as _;
use std::collections::BTreeMap;
use tabout::{tabulate_output, Alignment, Column};
use wezterm_client::client::Client;
use wezterm_term::TerminalSize;

#[derive(Debug, Parser, Clone, Copy)]
pub struct ListCommand {
    /// Controls the output format.
    /// "table" and "json" are possible formats.
    #[arg(long = "format", default_value = "table")]
    format: CliOutputFormatKind,
}

impl ListCommand {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let out = std::io::stdout();

        let mut output_items = vec![];
        let panes = client.list_panes().await?;

        anyhow::ensure!(
            panes.tab_titles.len() == panes.tabs.len() && panes.tab_vars.len() == panes.tabs.len(),
            "the server listed {} tabs with {} titles and {} sets of variables",
            panes.tabs.len(),
            panes.tab_titles.len(),
            panes.tab_vars.len()
        );
        for ((tabroot, tab_title), tab_vars) in panes
            .tabs
            .into_iter()
            .zip(panes.tab_titles.iter())
            .zip(panes.tab_vars.iter())
        {
            let mut cursor = tabroot.into_tree().cursor();

            loop {
                if let Some(entry) = cursor.leaf_mut() {
                    let window_title = panes
                        .window_titles
                        .get(&entry.window_id)
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let window_vars = panes.window_vars.get(&entry.window_id).ok_or_else(|| {
                        anyhow::anyhow!(
                            "the server listed no variables for window {}",
                            entry.window_id
                        )
                    })?;
                    output_items.push(CliListResultItem::from(
                        entry.clone(),
                        tab_title,
                        tab_vars,
                        window_title,
                        window_vars,
                    ));
                }
                match cursor.preorder_next() {
                    Ok(c) => cursor = c,
                    Err(_) => break,
                }
            }
        }
        match self.format {
            CliOutputFormatKind::Json => {
                let mut writer = serde_json::Serializer::pretty(out.lock());
                writer.collect_seq(output_items.iter())?;
            }
            CliOutputFormatKind::Table => {
                let cols = vec![
                    Column {
                        name: "WINID".to_string(),
                        alignment: Alignment::Right,
                    },
                    Column {
                        name: "TABID".to_string(),
                        alignment: Alignment::Right,
                    },
                    Column {
                        name: "PANEID".to_string(),
                        alignment: Alignment::Right,
                    },
                    Column {
                        name: "WORKSPACE".to_string(),
                        alignment: Alignment::Left,
                    },
                    Column {
                        name: "SIZE".to_string(),
                        alignment: Alignment::Left,
                    },
                    Column {
                        name: "TITLE".to_string(),
                        alignment: Alignment::Left,
                    },
                    Column {
                        name: "CWD".to_string(),
                        alignment: Alignment::Left,
                    },
                ];
                let data = output_items
                    .iter()
                    .map(|output_item| {
                        vec![
                            output_item.window_id.to_string(),
                            output_item.tab_id.to_string(),
                            output_item.pane_id.to_string(),
                            output_item.workspace.to_string(),
                            format!("{}x{}", output_item.size.cols, output_item.size.rows),
                            output_item.title.to_string(),
                            output_item.cwd.to_string(),
                        ]
                    })
                    .collect::<Vec<_>>();
                tabulate_output(&cols, &data, &mut std::io::stdout().lock())?;
            }
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct CliListResultPtySize {
    rows: usize,
    cols: usize,
    /// Pixel width of the pane, if known (can be zero)
    pixel_width: usize,
    /// Pixel height of the pane, if known (can be zero)
    pixel_height: usize,
    /// dpi of the pane, if known (can be zero)
    dpi: u32,
}

// This will be serialized to JSON via the 'List' command.
// As such it is intended to be a stable output format,
// Thus we need to be careful about both the fields and their types,
// herein as they are directly reflected in the output.
#[derive(serde::Serialize)]
struct CliListResultItem {
    window_id: mux::window::WindowId,
    tab_id: mux::tab::TabId,
    pane_id: mux::pane::PaneId,
    workspace: String,
    size: CliListResultPtySize,
    title: String,
    cwd: String,
    /// Cursor x coordinate from top left of non-scrollback pane area
    cursor_x: usize,
    /// Cursor y coordinate from top left of non-scrollback pane area
    cursor_y: usize,
    cursor_shape: termwiz::surface::CursorShape,
    cursor_visibility: termwiz::surface::CursorVisibility,
    /// Number of cols from the left of the tab area to the left of this pane
    left_col: usize,
    /// Number of rows from the top of the tab area to the top of this pane
    top_row: usize,
    tab_title: String,
    /// The tab's variables (`wezterm cli set-tab-var`)
    tab_vars: BTreeMap<String, String>,
    window_title: String,
    /// The window's variables (`wezterm cli set-window-var`)
    window_vars: BTreeMap<String, String>,
    is_active: bool,
    is_zoomed: bool,
    tty_name: Option<String>,
}

impl CliListResultItem {
    fn from(
        pane: mux::tab::PaneEntry,
        tab_title: &str,
        tab_vars: &BTreeMap<String, String>,
        window_title: &str,
        window_vars: &BTreeMap<String, String>,
    ) -> CliListResultItem {
        let mux::tab::PaneEntry {
            window_id,
            tab_id,
            pane_id,
            workspace,
            title,
            working_dir,
            cursor_pos,
            physical_top,
            left_col,
            top_row,
            is_active_pane,
            is_zoomed_pane,
            tty_name,
            size:
                TerminalSize {
                    rows,
                    cols,
                    pixel_width,
                    pixel_height,
                    dpi,
                },
            ..
        } = pane;

        CliListResultItem {
            window_id,
            tab_id,
            pane_id,
            workspace,
            size: CliListResultPtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
                dpi,
            },
            title,
            cwd: working_dir
                .as_ref()
                .map(|url| url.url.as_str())
                .unwrap_or("")
                .to_string(),
            cursor_x: cursor_pos.x,
            cursor_y: cursor_pos.y.saturating_sub(physical_top) as usize,
            cursor_shape: cursor_pos.shape,
            cursor_visibility: cursor_pos.visibility,
            left_col,
            top_row,
            tab_title: tab_title.to_string(),
            tab_vars: tab_vars.clone(),
            window_title: window_title.to_string(),
            window_vars: window_vars.clone(),
            is_active: is_active_pane,
            is_zoomed: is_zoomed_pane,
            tty_name,
        }
    }
}
